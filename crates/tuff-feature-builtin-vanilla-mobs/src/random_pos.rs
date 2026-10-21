//! Picking random places for a mob to go: ten tries around it, keeping the
//! one it likes most (animals prefer grass and light).

use rand::Rng;

use super::MobCore;
use super::path::static_path_type;
use tuff_core::world::entity::TickContext;
use tuff_world_model::BlockPos;

const ATTEMPTS: usize = 10;

fn random_direction(horizontal: i32, vertical: i32, cx: &mut TickContext) -> [i32; 3] {
    let x = cx.rng.gen_range(0..2 * horizontal + 1) - horizontal;
    let y = cx.rng.gen_range(0..2 * vertical + 1) - vertical;
    let z = cx.rng.gen_range(0..2 * horizontal + 1) - horizontal;
    [x, y, z]
}

/// The block `direction` away, pulled toward its home when it has one,
/// or `None` when that leaves a home it must keep to (`restrict`).
fn toward(
    mob: &MobCore,
    horizontal: i32,
    restrict: bool,
    direction: [i32; 3],
    cx: &mut TickContext,
) -> Option<BlockPos> {
    let [x, y, z] = mob.base.position;
    let (mut dx, mut dz) = (f64::from(direction[0]), f64::from(direction[2]));
    if let Some(home) = mob.home
        && horizontal > 1
    {
        let reach = f64::from(horizontal);
        let shift = cx.rng.r#gen::<f64>() * reach / 2.0;
        dx += if x > f64::from(home.pos.x) {
            -shift
        } else {
            shift
        };
        let shift = cx.rng.r#gen::<f64>() * reach / 2.0;
        dz += if z > f64::from(home.pos.z) {
            -shift
        } else {
            shift
        };
    }
    let pos = BlockPos::new(
        (dx + x).floor() as i32,
        (f64::from(direction[1]) + y).floor() as i32,
        (dz + z).floor() as i32,
    );
    (!restrict || mob.within_home(pos)).then_some(pos)
}

fn outside_limits(pos: BlockPos, mob: &MobCore) -> bool {
    pos.y < mob.min_y || pos.y > mob.max_y
}

fn has_malus(mob: &MobCore, pos: BlockPos, cx: &TickContext) -> bool {
    mob.path_malus(static_path_type(pos, cx.block, mob.min_y)) != 0.0
}

fn best(
    mob: &MobCore,
    cx: &mut TickContext,
    candidate: impl FnMut(&MobCore, &mut TickContext) -> Option<BlockPos>,
) -> Option<[f64; 3]> {
    best_by(mob, cx, candidate, |mob, pos, cx| {
        mob.walk_target_value(pos, cx)
    })
}

fn best_by(
    mob: &MobCore,
    cx: &mut TickContext,
    mut candidate: impl FnMut(&MobCore, &mut TickContext) -> Option<BlockPos>,
    score: impl Fn(&MobCore, BlockPos, &TickContext) -> f32,
) -> Option<[f64; 3]> {
    let mut best: Option<(f32, BlockPos)> = None;
    for _ in 0..ATTEMPTS {
        let Some(pos) = candidate(mob, cx) else {
            continue;
        };
        let value = score(mob, pos, cx);
        if best.is_none_or(|(b, _)| value > b) {
            best = Some((value, pos));
        }
    }
    best.map(|(_, pos)| [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5])
}

/// Anywhere within reach it can stand without a path malus.
pub fn default_pos(
    mob: &MobCore,
    horizontal: i32,
    vertical: i32,
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    let restrict = mob.home_restricts(f64::from(horizontal));
    best(mob, cx, |mob, cx| {
        let direction = random_direction(horizontal, vertical, cx);
        let pos = toward(mob, horizontal, restrict, direction, cx)?;
        (!outside_limits(pos, mob)
            && mob.is_stable_destination(pos, cx)
            && !has_malus(mob, pos, cx))
        .then_some(pos)
    })
}

/// Anywhere within reach, rerolled up to ten times until it lands in a
/// block water mobs may path through (the last roll is kept either way).
pub fn swimmable_pos(
    mob: &MobCore,
    horizontal: i32,
    vertical: i32,
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    let swimmable = |pos: [f64; 3], cx: &TickContext| {
        (cx.block)(super::physics::block_pos(pos)).is_some_and(|s| s.is_water_pathfindable())
    };
    let mut target = default_pos(mob, horizontal, vertical, cx);
    let mut rerolls = 0;
    while let Some(pos) = target
        && !swimmable(pos, cx)
        && rerolls < 10
    {
        rerolls += 1;
        target = default_pos(mob, horizontal, vertical, cx);
    }
    target
}

/// Where a swimming mob's brain sends it: ever wider swimmable spots,
/// stopping at the first that is not in water; past the first, each tier
/// just stretches the direction the previous one found.
pub fn swim_target(mob: &MobCore, cx: &mut TickContext) -> Option<[f64; 3]> {
    const TIERS: [(i32, i32); 6] = [(1, 1), (3, 3), (5, 5), (6, 5), (7, 7), (10, 7)];
    let wet = |pos: [f64; 3], cx: &TickContext| {
        (cx.block)(super::physics::block_pos(pos)).is_some_and(|s| !s.fluid_state().is_empty())
    };
    let mut fallback: Option<[f64; 3]> = None;
    let mut target = None;
    for (horizontal, vertical) in TIERS {
        target = match fallback {
            None => swimmable_pos(mob, horizontal, vertical, cx),
            Some(previous) => {
                let [x, y, z] = mob.base.position;
                let d = [previous[0] - x, previous[1] - y, previous[2] - z];
                let length = d.iter().map(|c| c * c).sum::<f64>().sqrt();
                let unit = if length < 1.0e-5 {
                    [0.0; 3]
                } else {
                    d.map(|c| c / length)
                };
                Some([
                    x + unit[0] * f64::from(horizontal),
                    y + unit[1] * f64::from(vertical),
                    z + unit[2] * f64::from(horizontal),
                ])
            }
        };
        match target {
            Some(pos) if wet(pos, cx) => fallback = Some(pos),
            _ => return fallback,
        }
    }
    target
}

/// Somewhere on land: candidates in solid blocks move up out of them, and
/// water or a malus rules a spot out.
pub fn land_pos(
    mob: &MobCore,
    horizontal: i32,
    vertical: i32,
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    land_pos_scored(
        mob,
        horizontal,
        vertical,
        |mob, pos, cx| mob.walk_target_value(pos, cx),
        cx,
    )
}

/// As [`land_pos`], keeping the candidate `score` likes most.
pub fn land_pos_scored(
    mob: &MobCore,
    horizontal: i32,
    vertical: i32,
    score: impl Fn(&MobCore, BlockPos, &TickContext) -> f32,
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    let restrict = mob.home_restricts(f64::from(horizontal));
    best_by(
        mob,
        cx,
        |mob, cx| {
            let direction = random_direction(horizontal, vertical, cx);
            let pos = toward(mob, horizontal, restrict, direction, cx)?;
            if outside_limits(pos, mob) || !mob.is_stable_destination(pos, cx) {
                return None;
            }
            let solid = |p: BlockPos| (cx.block)(p).is_some_and(|s| s.is_solid());
            let mut up = pos;
            if solid(up) {
                up.y += 1;
                while up.y <= mob.max_y && solid(up) {
                    up.y += 1;
                }
            }
            let water = (cx.block)(up).is_some_and(|s| {
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::fluid::key::WATER,
                    s.fluid_state().fluid(),
                )
            });
            (!water && !has_malus(mob, up, cx)).then_some(up)
        },
        score,
    )
}

/// A random direction within 90 degrees of `toward` (x and z), as whole
/// blocks at most `horizontal` away.
fn direction_within(
    horizontal: i32,
    vertical: i32,
    toward: [f64; 2],
    cx: &mut TickContext,
) -> [i32; 3] {
    direction_within_angle(
        horizontal,
        vertical,
        toward,
        std::f64::consts::FRAC_PI_2,
        cx,
    )
}

/// A random direction within `max_angle` radians of `toward`.
fn direction_within_angle(
    horizontal: i32,
    vertical: i32,
    toward: [f64; 2],
    max_angle: f64,
    cx: &mut TickContext,
) -> [i32; 3] {
    let base = toward[1].atan2(toward[0]);
    let angle = base + (cx.rng.r#gen::<f64>() * 2.0 - 1.0) * max_angle;
    let distance = cx.rng.r#gen::<f64>().sqrt() * std::f64::consts::SQRT_2 * horizontal as f64;
    let x = (angle.cos() * distance).round() as i32;
    let z = (angle.sin() * distance).round() as i32;
    let y = cx.rng.gen_range(0..2 * vertical + 1) - vertical;
    [
        x.clamp(-horizontal, horizontal),
        y,
        z.clamp(-horizontal, horizontal),
    ]
}

/// Somewhere on land toward `target`: as [`land_pos`], but within 90
/// degrees of the target's direction.
pub fn land_pos_towards(
    mob: &MobCore,
    horizontal: i32,
    vertical: i32,
    target: [f64; 3],
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    let [x, _, z] = mob.base.position;
    let direction = [target[0] - x, target[2] - z];
    let restrict = mob.home_restricts(f64::from(horizontal));
    best(mob, cx, |mob, cx| {
        let direction = direction_within(horizontal, vertical, direction, cx);
        let pos = toward(mob, horizontal, restrict, direction, cx)?;
        if outside_limits(pos, mob) || !mob.is_stable_destination(pos, cx) {
            return None;
        }
        let solid = |p: BlockPos| (cx.block)(p).is_some_and(|s| s.is_solid());
        let mut up = pos;
        while up.y <= mob.max_y && solid(up) {
            up.y += 1;
        }
        let water = (cx.block)(up).is_some_and(|s| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::fluid::key::WATER,
                s.fluid_state().fluid(),
            )
        });
        (!water && !has_malus(mob, up, cx)).then_some(up)
    })
}

/// Somewhere on land away from `from`.
pub fn land_pos_away(
    mob: &MobCore,
    horizontal: i32,
    vertical: i32,
    from: [f64; 3],
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    let [x, _, z] = mob.base.position;
    let away = [x - from[0], z - from[2]];
    let restrict = mob.home_restricts(f64::from(horizontal));
    best(mob, cx, |mob, cx| {
        let direction = direction_within(horizontal, vertical, away, cx);
        let pos = toward(mob, horizontal, restrict, direction, cx)?;
        if outside_limits(pos, mob) || !mob.is_stable_destination(pos, cx) {
            return None;
        }
        let water = (cx.block)(pos).is_some_and(|s| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::fluid::key::WATER,
                s.fluid_state().fluid(),
            )
        });
        (!water && !has_malus(mob, pos, cx)).then_some(pos)
    })
}

/// As [`land_pos_away`], at between `min` and `max` blocks from the mob.
pub fn land_pos_away_between(
    mob: &MobCore,
    min: f64,
    max: f64,
    vertical: i32,
    from: [f64; 3],
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    let [x, _, z] = mob.base.position;
    let mut away = [x - from[0], z - from[2]];
    if away[0] == 0.0 && away[1] == 0.0 {
        away = [cx.rng.r#gen::<f64>() - 0.5, cx.rng.r#gen::<f64>() - 0.5];
    }
    let horizontal = max.ceil() as i32;
    let restrict = mob.home_restricts(max);
    best(mob, cx, |mob, cx| {
        let center = away[1].atan2(away[0]) - std::f64::consts::FRAC_PI_2;
        let angle =
            center + (f64::from(cx.rng.r#gen::<f32>()) * 2.0 - 1.0) * std::f64::consts::FRAC_PI_2;
        let distance =
            (min + (max - min) * cx.rng.r#gen::<f64>().sqrt()) * std::f64::consts::SQRT_2;
        let (dx, dz) = (-distance * angle.sin(), distance * angle.cos());
        if dx.abs() > max || dz.abs() > max {
            return None;
        }
        let dy = cx.rng.gen_range(0..2 * vertical + 1) - vertical;
        let direction = [dx.floor() as i32, dy, dz.floor() as i32];
        let pos = toward(mob, horizontal, restrict, direction, cx)?;
        if outside_limits(pos, mob) || !mob.is_stable_destination(pos, cx) {
            return None;
        }
        let water = (cx.block)(pos).is_some_and(|s| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::fluid::key::WATER,
                s.fluid_state().fluid(),
            )
        });
        (!water && !has_malus(mob, pos, cx)).then_some(pos)
    })
}

/// Anywhere within reach toward `target`.
pub fn pos_towards(
    mob: &MobCore,
    horizontal: i32,
    vertical: i32,
    target: [f64; 3],
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    let [x, _, z] = mob.base.position;
    pos_in_direction(
        mob,
        horizontal,
        vertical,
        [target[0] - x, target[2] - z],
        cx,
    )
}

/// Anywhere within reach toward `target`, within `max_angle` radians of
/// its direction.
pub fn pos_towards_within(
    mob: &MobCore,
    horizontal: i32,
    vertical: i32,
    target: [f64; 3],
    max_angle: f64,
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    let [x, _, z] = mob.base.position;
    let direction = [target[0] - x, target[2] - z];
    let restrict = mob.home_restricts(f64::from(horizontal));
    best(mob, cx, |mob, cx| {
        let direction = direction_within_angle(horizontal, vertical, direction, max_angle, cx);
        let pos = toward(mob, horizontal, restrict, direction, cx)?;
        (!outside_limits(pos, mob)
            && mob.is_stable_destination(pos, cx)
            && !has_malus(mob, pos, cx))
        .then_some(pos)
    })
}

/// Anywhere within reach away from `from`.
pub fn pos_away(
    mob: &MobCore,
    horizontal: i32,
    vertical: i32,
    from: [f64; 3],
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    let [x, _, z] = mob.base.position;
    pos_in_direction(mob, horizontal, vertical, [x - from[0], z - from[2]], cx)
}

fn pos_in_direction(
    mob: &MobCore,
    horizontal: i32,
    vertical: i32,
    direction: [f64; 2],
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    let restrict = mob.home_restricts(f64::from(horizontal));
    best(mob, cx, |mob, cx| {
        let direction = direction_within(horizontal, vertical, direction, cx);
        let pos = toward(mob, horizontal, restrict, direction, cx)?;
        (!outside_limits(pos, mob)
            && mob.is_stable_destination(pos, cx)
            && !has_malus(mob, pos, cx))
        .then_some(pos)
    })
}

/// A direction within `max_angle` radians of the view direction `view`
/// (x, z), `flying_height` blocks higher on average.
fn direction_around_view(
    horizontal: i32,
    vertical: i32,
    flying_height: i32,
    view: [f64; 2],
    max_angle: f64,
    cx: &mut TickContext,
) -> Option<[i32; 3]> {
    let center = view[1].atan2(view[0]) - std::f64::consts::FRAC_PI_2;
    let angle = center + f64::from(cx.rng.r#gen::<f32>() * 2.0 - 1.0) * max_angle;
    let distance = cx.rng.r#gen::<f64>().sqrt() * f64::from(horizontal) * std::f64::consts::SQRT_2;
    let (x, z) = (-distance * angle.sin(), distance * angle.cos());
    if x.abs() > f64::from(horizontal) || z.abs() > f64::from(horizontal) {
        return None;
    }
    let y = cx.rng.gen_range(0..2 * vertical + 1) - vertical + flying_height;
    Some([x.floor() as i32, y, z.floor() as i32])
}

/// Where a flier idles to: hovering one to three blocks above ground
/// ahead of where it looks, else anywhere in the air or water there.
pub fn flying_pos(mob: &MobCore, view: [f64; 3], cx: &mut TickContext) -> Option<[f64; 3]> {
    hover_pos(mob, view, cx).or_else(|| air_and_water_pos(mob, 8, 4, -2, view, cx))
}

/// One to three blocks above ground ahead of `view`, never in water.
pub fn hover_pos(mob: &MobCore, view: [f64; 3], cx: &mut TickContext) -> Option<[f64; 3]> {
    let view = [view[0], view[2]];
    let restrict = mob.home_restricts(8.0);
    best(mob, cx, |mob, cx| {
        let direction = direction_around_view(8, 7, 0, view, std::f64::consts::FRAC_PI_2, cx)?;
        let mut pos = toward(mob, 8, restrict, direction, cx)?;
        if outside_limits(pos, mob) || !mob.is_stable_destination(pos, cx) {
            return None;
        }
        let solid = |p: BlockPos| (cx.block)(p).is_some_and(|s| s.is_solid());
        let above = cx.rng.gen_range(1..=3);
        if solid(pos) {
            pos.y += 1;
            while pos.y <= mob.max_y && solid(pos) {
                pos.y += 1;
            }
            let first = pos.y;
            while pos.y <= mob.max_y && pos.y - first < above {
                pos.y += 1;
                if solid(pos) {
                    pos.y -= 1;
                    break;
                }
            }
        }
        let water = (cx.block)(pos).is_some_and(|s| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::fluid::key::WATER,
                s.fluid_state().fluid(),
            )
        });
        (!water && !has_malus(mob, pos, cx)).then_some(pos)
    })
}

/// Anywhere in the air or water within reach ahead of `view`, shifted by
/// `flying_height` blocks.
pub fn air_and_water_pos(
    mob: &MobCore,
    horizontal: i32,
    vertical: i32,
    flying_height: i32,
    view: [f64; 3],
    cx: &mut TickContext,
) -> Option<[f64; 3]> {
    let view = [view[0], view[2]];
    let restrict = mob.home_restricts(f64::from(horizontal));
    best(mob, cx, |mob, cx| {
        let direction = direction_around_view(
            horizontal,
            vertical,
            flying_height,
            view,
            std::f64::consts::FRAC_PI_2,
            cx,
        )?;
        let mut pos = toward(mob, horizontal, restrict, direction, cx)?;
        if outside_limits(pos, mob) {
            return None;
        }
        let solid = |p: BlockPos| (cx.block)(p).is_some_and(|s| s.is_solid());
        while solid(pos) && pos.y <= mob.max_y {
            pos.y += 1;
        }
        (!has_malus(mob, pos, cx)).then_some(pos)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_context;

    #[tuff_core::watchdog]
    #[test]
    fn a_home_keeps_random_spots_inside_it() {
        with_context(|cow, cx| {
            let mut outside = 0;
            for _ in 0..40 {
                if let Some(at) = default_pos(cow, 10, 3, cx)
                    && tuff_core::world::distance_sq(at, cow.base.position) > 16.0
                {
                    outside += 1;
                }
            }
            assert!(outside > 0, "without a home it roams");
            cow.home = Some(crate::Home {
                pos: BlockPos::new(0, 64, 0),
                radius: 4,
            });
            for _ in 0..40 {
                if let Some(at) = default_pos(cow, 10, 3, cx) {
                    let pos = crate::physics::block_pos(at);
                    assert!(cow.within_home(pos), "{at:?}");
                }
            }
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn flying_positions_stay_within_reach_of_the_flier() {
        with_context(|cow, cx| {
            let mut found = 0;
            for _ in 0..40 {
                if let Some(at) = flying_pos(cow, [0.0, 0.0, 1.0], cx) {
                    found += 1;
                    let [x, y, z] = cow.base.position;
                    assert!(
                        (at[0] - x).abs() <= 9.0 && (at[2] - z).abs() <= 9.0,
                        "{at:?}"
                    );
                    assert!((at[1] - y).abs() <= 8.0, "{at:?}");
                }
            }
            assert!(found > 0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn air_and_water_positions_shift_with_the_flying_height() {
        with_context(|bird, cx| {
            let mut mean = |height: i32| {
                let (mut found, mut total) = (0, 0.0);
                for _ in 0..300 {
                    if let Some(at) = air_and_water_pos(bird, 10, 7, height, [0.0, 0.0, 1.0], cx) {
                        found += 1;
                        total += at[1];
                        assert!((at[0] - bird.base.position[0]).abs() <= 11.0, "{at:?}");
                    }
                }
                assert!(found > 0);
                total / f64::from(found)
            };
            let (low, high) = (mean(-2), mean(6));
            assert!(high > low + 3.0, "{low} {high}");
        });
    }
}
