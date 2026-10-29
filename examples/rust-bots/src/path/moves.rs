// SPDX-License-Identifier: MIT
use super::{
    ASCEND_TICKS, BREAK_OVERHEAD_TICKS, BRIDGE_TICKS, CLIMB_DOWN_TICKS, CLIMB_UP_TICKS,
    DANGER_NEIGHBOR_TICKS, Edge, FALL_DAMAGE_TICKS_PER_BLOCK, MAX_BREAKS_PER_STEP, MAX_WATER_FALL,
    MoveKind, PARKOUR_JUMP_TICKS, PILLAR_TICKS, PLACE_TICKS, Params, Pos, SAFE_FALL, SPRINT_TICKS,
    SUBMERGED_TICKS, SWIM_TICKS, WALK_TICKS,
};
use crate::world::{Blocks, Class, Pending};

const FLAT: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const DIAGONAL: [(i32, i32); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];

pub fn default_moves() -> Vec<Box<dyn MoveKind>> {
    vec![
        Box::new(Traverse),
        Box::new(Diagonal),
        Box::new(Ascend),
        Box::new(Descend),
        Box::new(Climb),
        Box::new(Pillar),
        Box::new(Parkour),
    ]
}

/// Ticks to fall `blocks` blocks from rest.
pub fn fall_ticks(blocks: u32) -> f64 {
    let (mut y, mut v, mut ticks) = (0.0_f64, 0.0_f64, 0u32);
    while y > -f64::from(blocks) && ticks < 400 {
        v = (v - 0.08) * 0.98;
        y += v;
        ticks += 1;
    }
    f64::from(ticks)
}

fn passable(w: &mut dyn Blocks, x: i32, y: i32, z: i32) -> Result<bool, Pending> {
    Ok(w.class_at(x, y, z)?.passable())
}

/// Feet and head cells are free to occupy.
pub(super) fn fits(w: &mut dyn Blocks, x: i32, y: i32, z: i32) -> Result<bool, Pending> {
    Ok(passable(w, x, y, z)? && passable(w, x, y + 1, z)?)
}

/// Something holds a body at the cell: ground, a ladder or water.
pub(super) fn supported(w: &mut dyn Blocks, x: i32, y: i32, z: i32) -> Result<bool, Pending> {
    let feet = w.class_at(x, y, z)?;
    if feet.climbable() || feet.water() {
        return Ok(true);
    }
    Ok(w.class_at(x, y - 1, z)?.standable())
}

fn hazard(w: &mut dyn Blocks, x: i32, y: i32, z: i32) -> Result<f64, Pending> {
    let mut near = 0.0;
    for (dx, dz) in FLAT {
        for dy in [-1, 0, 1] {
            if w.class_at(x + dx, y + dy, z + dz)?.danger() {
                near += DANGER_NEIGHBOR_TICKS;
                break;
            }
        }
    }
    Ok(near)
}

fn step_ticks(w: &mut dyn Blocks, params: &Params, to: Pos, from: Pos) -> Result<f64, Pending> {
    let in_water =
        w.class_at(to.0, to.1, to.2)?.water() || w.class_at(from.0, from.1, from.2)?.water();
    Ok(if in_water {
        SWIM_TICKS + submerged_ticks(w, to)?
    } else {
        params.walk_ticks()
    })
}

/// Extra cost of a cell whose head is under water: the bot is spending air there.
fn submerged_ticks(w: &mut dyn Blocks, at: Pos) -> Result<f64, Pending> {
    let feet = w.class_at(at.0, at.1, at.2)?;
    Ok(
        if feet.water() && w.class_at(at.0, at.1 + 1, at.2)?.water() {
            SUBMERGED_TICKS
        } else {
            0.0
        },
    )
}

fn push(out: &mut Vec<Edge>, params: &Params, to: Pos, cost: f64) {
    push_placing(out, params, to, cost, 0);
}

fn push_placing(out: &mut Vec<Edge>, params: &Params, to: Pos, cost: f64, place: u32) {
    if !params.avoids(to) && place <= params.throwaway {
        out.push(Edge { to, cost, place });
    }
}

/// Opening the cell lets nothing fall or flow onto the bot: no falling
/// block or fluid above it (unless that is opened too) and no fluid beside.
fn safe_to_open(w: &mut dyn Blocks, cells: &[Pos], (x, y, z): Pos) -> Result<bool, Pending> {
    let above = (x, y + 1, z);
    if !cells.contains(&above) {
        let a = w.class_at(x, y + 1, z)?;
        if a.falling() || a.fluid() {
            return Ok(false);
        }
    }
    for (dx, dz) in FLAT {
        if w.class_at(x + dx, y, z + dz)?.fluid() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Ticks to clear every blocking cell of `cells`, zero when all are open;
/// `None` when a cell cannot or may not be broken, or when more than
/// [`MAX_BREAKS_PER_STEP`] block the way.
pub fn break_cost(w: &mut dyn Blocks, p: &Params, cells: &[Pos]) -> Result<Option<f64>, Pending> {
    let mut cost = 0.0;
    let mut broken = 0;
    for &(x, y, z) in cells {
        let class = w.class_at(x, y, z)?;
        if class.passable() {
            continue;
        }
        let Some(breaking) = &p.breaking else {
            return Ok(None);
        };
        broken += 1;
        if class.danger() || broken > MAX_BREAKS_PER_STEP {
            return Ok(None);
        }
        let Some(state) = w.state_at(x, y, z)? else {
            return Ok(None);
        };
        let Some(ticks) = breaking.ticks(&state) else {
            return Ok(None);
        };
        if !safe_to_open(w, cells, (x, y, z))? {
            return Ok(None);
        }
        cost += f64::from(ticks) + BREAK_OVERHEAD_TICKS;
    }
    Ok(Some(cost))
}

/// A throwaway block can go into the cell under `to`, placed against the
/// side of the block the bot stands on at `from`.
fn can_fill_below(w: &mut dyn Blocks, p: &Params, from: Pos, to: Pos) -> Result<bool, Pending> {
    if p.throwaway == 0 {
        return Ok(false);
    }
    let feet = w.class_at(from.0, from.1, from.2)?;
    if feet.climbable() || feet.water() {
        return Ok(false);
    }
    let below = w.class_at(to.0, to.1 - 1, to.2)?;
    Ok(below.passable() && !below.climbable() && !below.danger())
}

pub struct Traverse;

impl MoveKind for Traverse {
    fn expand(
        &self,
        w: &mut dyn Blocks,
        p: &Params,
        from: Pos,
        out: &mut Vec<Edge>,
    ) -> Result<(), Pending> {
        let (x, y, z) = from;
        for (dx, dz) in FLAT {
            let to = (x + dx, y, z + dz);
            let Some(dig) = break_cost(w, p, &[(to.0, y + 1, to.2), to])? else {
                continue;
            };
            let hazard = hazard(w, to.0, to.1, to.2)?;
            if supported(w, to.0, to.1, to.2)? {
                let cost = step_ticks(w, p, to, from)? + dig + hazard;
                push(out, p, to, cost);
            } else if can_fill_below(w, p, from, to)? {
                let cost = BRIDGE_TICKS + PLACE_TICKS + dig + hazard;
                push_placing(out, p, to, cost, 1);
            }
        }
        Ok(())
    }
}

pub struct Diagonal;

impl MoveKind for Diagonal {
    fn expand(
        &self,
        w: &mut dyn Blocks,
        p: &Params,
        from: Pos,
        out: &mut Vec<Edge>,
    ) -> Result<(), Pending> {
        let (x, y, z) = from;
        for (dx, dz) in DIAGONAL {
            let to = (x + dx, y, z + dz);
            if fits(w, to.0, to.1, to.2)?
                && supported(w, to.0, to.1, to.2)?
                && fits(w, x + dx, y, z)?
                && fits(w, x, y, z + dz)?
            {
                let cost = step_ticks(w, p, to, from)? * std::f64::consts::SQRT_2
                    + hazard(w, to.0, to.1, to.2)?;
                push(out, p, to, cost);
            }
        }
        Ok(())
    }
}

pub struct Ascend;

impl MoveKind for Ascend {
    fn expand(
        &self,
        w: &mut dyn Blocks,
        p: &Params,
        from: Pos,
        out: &mut Vec<Edge>,
    ) -> Result<(), Pending> {
        let (x, y, z) = from;
        for (dx, dz) in FLAT {
            let to = (x + dx, y + 1, z + dz);
            if !w.class_at(to.0, to.1 - 1, to.2)?.standable() {
                continue;
            }
            let cells = [(x, y + 2, z), (to.0, y + 2, to.2), to];
            let Some(dig) = break_cost(w, p, &cells)? else {
                continue;
            };
            let cost = p.walk_ticks() + ASCEND_TICKS + dig + hazard(w, to.0, to.1, to.2)?;
            push(out, p, to, cost);
        }
        Ok(())
    }
}

/// Walks off an edge and drops to the first landing: one block is a plain
/// descend. Water landings are free; others are limited by the policy and
/// cost extra beyond a safe fall.
pub struct Descend;

impl MoveKind for Descend {
    fn expand(
        &self,
        w: &mut dyn Blocks,
        p: &Params,
        from: Pos,
        out: &mut Vec<Edge>,
    ) -> Result<(), Pending> {
        let (x, y, z) = from;
        for (dx, dz) in FLAT {
            let (nx, nz) = (x + dx, z + dz);
            if supported(w, nx, y, nz)? {
                dig_down(w, p, (nx, y, nz), out)?;
                continue;
            }
            let Some(dig) = break_cost(w, p, &[(nx, y + 1, nz), (nx, y, nz)])? else {
                continue;
            };
            for drop in 1..=MAX_WATER_FALL {
                let ly = y - drop as i32;
                if !passable(w, nx, ly, nz)? {
                    break;
                }
                let feet = w.class_at(nx, ly, nz)?;
                let ground = w.class_at(nx, ly - 1, nz)?.standable();
                if !(feet.water() || feet.climbable() || ground) {
                    continue;
                }
                let free = feet.water() || feet.climbable();
                if !free && drop > p.max_fall {
                    break;
                }
                let damage = if free {
                    0.0
                } else {
                    f64::from(drop.saturating_sub(SAFE_FALL)) * FALL_DAMAGE_TICKS_PER_BLOCK
                };
                let cost =
                    p.walk_ticks() + fall_ticks(drop) + damage + dig + hazard(w, nx, ly, nz)?;
                push(out, p, (nx, ly, nz), cost);
                break;
            }
        }
        Ok(())
    }
}

/// Steps down one block into a cell that is solid now: breaks the way
/// into it and lands on the block under it.
fn dig_down(w: &mut dyn Blocks, p: &Params, at: Pos, out: &mut Vec<Edge>) -> Result<(), Pending> {
    let (nx, y, nz) = at;
    let feet = w.class_at(nx, y, nz)?;
    if p.breaking.is_none()
        || feet.climbable()
        || feet.water()
        || w.class_at(nx, y - 1, nz)?.passable()
        || !w.class_at(nx, y - 2, nz)?.standable()
    {
        return Ok(());
    }
    let cells = [(nx, y + 1, nz), (nx, y, nz), (nx, y - 1, nz)];
    if let Some(dig) = break_cost(w, p, &cells)? {
        let to = (nx, y - 1, nz);
        let cost = p.walk_ticks() + fall_ticks(1) + dig + hazard(w, nx, y - 1, nz)?;
        push(out, p, to, cost);
    }
    Ok(())
}

/// Jumps and places a throwaway block under the feet, one block up.
pub struct Pillar;

impl MoveKind for Pillar {
    fn expand(
        &self,
        w: &mut dyn Blocks,
        p: &Params,
        from: Pos,
        out: &mut Vec<Edge>,
    ) -> Result<(), Pending> {
        let (x, y, z) = from;
        if p.throwaway == 0 {
            return Ok(());
        }
        let feet = w.class_at(x, y, z)?;
        if feet.climbable() || feet.water() || !passable(w, x, y + 1, z)? {
            return Ok(());
        }
        if let Some(dig) = break_cost(w, p, &[(x, y + 2, z)])? {
            push_placing(out, p, (x, y + 1, z), PILLAR_TICKS + PLACE_TICKS + dig, 1);
        }
        Ok(())
    }
}

/// Up and down a ladder, vine or column of water.
pub struct Climb;

impl MoveKind for Climb {
    fn expand(
        &self,
        w: &mut dyn Blocks,
        p: &Params,
        from: Pos,
        out: &mut Vec<Edge>,
    ) -> Result<(), Pending> {
        let (x, y, z) = from;
        let here: Class = w.class_at(x, y, z)?;
        if !(here.climbable() || here.water()) {
            return Ok(());
        }
        let up = w.class_at(x, y + 1, z)?;
        if (up.climbable() || up.water()) && passable(w, x, y + 2, z)? {
            let cost = if up.water() {
                SWIM_TICKS + submerged_ticks(w, (x, y + 1, z))?
            } else {
                CLIMB_UP_TICKS
            };
            push(out, p, (x, y + 1, z), cost);
        }
        let down = w.class_at(x, y - 1, z)?;
        if down.climbable() || down.water() {
            let cost = if down.water() {
                SWIM_TICKS + submerged_ticks(w, (x, y - 1, z))?
            } else {
                CLIMB_DOWN_TICKS
            };
            push(out, p, (x, y - 1, z), cost);
        }
        Ok(())
    }
}

/// Blocks of air jumped over between `from` and `to` when the step is a
/// parkour jump along one axis, `None` for any other step.
pub fn parkour_gap(from: Pos, to: Pos) -> Option<i32> {
    let (dx, dz) = ((to.0 - from.0).abs(), (to.2 - from.2).abs());
    let reach = dx.max(dz);
    (dx.min(dz) == 0 && reach >= 2).then_some(reach - 1)
}

fn clear(w: &mut dyn Blocks, x: i32, y: i32, z: i32, above: i32) -> Result<bool, Pending> {
    for dy in 0..=above {
        if !passable(w, x, y + dy, z)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Sprint-jumps over gaps along one axis: 1 to 3 blocks on the level, 1 to
/// 2 blocks onto a block one higher. The longer jumps need sprint and a
/// block of run-up behind the take-off.
pub struct Parkour;

impl MoveKind for Parkour {
    fn expand(
        &self,
        w: &mut dyn Blocks,
        p: &Params,
        from: Pos,
        out: &mut Vec<Edge>,
    ) -> Result<(), Pending> {
        let (x, y, z) = from;
        if !p.allow_parkour
            || !w.class_at(x, y - 1, z)?.standable()
            || w.class_at(x, y, z)?.water()
            || !clear(w, x, y, z, 2)?
        {
            return Ok(());
        }
        for (dx, dz) in FLAT {
            let first = (x + dx, z + dz);
            if !fits(w, first.0, y, first.1)? || supported(w, first.0, y, first.1)? {
                continue;
            }
            let run_up =
                fits(w, x - dx, y, z - dz)? && w.class_at(x - dx, y - 1, z - dz)?.standable();
            for reach in 2..=4 {
                let (nx, nz) = (x + dx * reach, z + dz * reach);
                let gap = reach - 1;
                let between = (nx - dx, nz - dz);
                if !clear(w, between.0, y, between.1, 2)? {
                    break;
                }
                if gap >= 2 && !p.allow_sprint {
                    break;
                }
                let long = gap == 3;
                if gap <= 2
                    && clear(w, between.0, y, between.1, 3)?
                    && clear(w, x, y, z, 3)?
                    && w.class_at(nx, y, nz)?.standable()
                    && fits(w, nx, y + 1, nz)?
                    && passable(w, nx, y + 3, nz)?
                    && (gap == 1 || run_up)
                {
                    let cost =
                        f64::from(reach) * SPRINT_TICKS + ASCEND_TICKS + hazard(w, nx, y + 1, nz)?;
                    push(out, p, (nx, y + 1, nz), cost);
                }
                if fits(w, nx, y, nz)? && supported(w, nx, y, nz)? {
                    if (!long || run_up) && !w.class_at(nx, y, nz)?.water() {
                        let speed = if gap == 1 { WALK_TICKS } else { SPRINT_TICKS };
                        let cost =
                            f64::from(reach) * speed + PARKOUR_JUMP_TICKS + hazard(w, nx, y, nz)?;
                        push(out, p, (nx, y, nz), cost);
                    }
                    break;
                }
                if !fits(w, nx, y, nz)? {
                    break;
                }
            }
        }
        Ok(())
    }
}
