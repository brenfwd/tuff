//! Danger avoidance: hostile mobs and projectiles become extra boxes the
//! planner keeps out of. Free of host calls so the decisions are plain
//! functions.

use tuff_bots_api::control::{Aabb, Vec3};

use crate::path::Params;
use crate::world::name_of;

/// Blocks kept clear around a hostile mob.
pub const MOB_MARGIN: f64 = 3.0;
/// Blocks kept clear around a projectile's line of flight.
pub const PROJECTILE_MARGIN: f64 = 1.5;
const PROJECTILE_LOOKAHEAD: f64 = 12.0;

const HOSTILES: &[&str] = &[
    "zombie",
    "husk",
    "drowned",
    "skeleton",
    "stray",
    "creeper",
    "spider",
    "cave_spider",
    "witch",
    "pillager",
    "vindicator",
    "ravager",
    "phantom",
    "blaze",
    "wither_skeleton",
    "piglin_brute",
    "hoglin",
    "zoglin",
    "slime",
    "magma_cube",
    "enderman",
];

const PROJECTILES: &[&str] = &[
    "arrow",
    "spectral_arrow",
    "fireball",
    "small_fireball",
    "trident",
];

pub fn is_hostile(kind: &str) -> bool {
    HOSTILES.contains(&name_of(kind))
}

pub fn is_projectile(kind: &str) -> bool {
    PROJECTILES.contains(&name_of(kind))
}

/// A nearby entity: its type, feet position and velocity per tick.
pub struct Nearby {
    pub kind: String,
    pub pos: Vec3,
    pub velocity: Vec3,
}

fn around(c: &Vec3, margin: f64, height: f64) -> Aabb {
    Aabb {
        min: Vec3 {
            x: c.x - margin,
            y: c.y - 1.0,
            z: c.z - margin,
        },
        max: Vec3 {
            x: c.x + margin,
            y: c.y + height,
            z: c.z + margin,
        },
    }
}

/// The boxes to stay out of for these entities. A projectile blocks the
/// stretch of its flight ahead, a hostile mob a margin around it.
pub fn danger_boxes(nearby: &[Nearby]) -> Vec<Aabb> {
    let mut boxes = Vec::new();
    for e in nearby {
        if is_hostile(&e.kind) {
            boxes.push(around(&e.pos, MOB_MARGIN, 2.0));
        } else if is_projectile(&e.kind) {
            let speed = (e.velocity.x.powi(2) + e.velocity.y.powi(2) + e.velocity.z.powi(2)).sqrt();
            if speed < 0.05 {
                continue;
            }
            let end = Vec3 {
                x: e.pos.x + e.velocity.x / speed * PROJECTILE_LOOKAHEAD,
                y: e.pos.y + e.velocity.y / speed * PROJECTILE_LOOKAHEAD,
                z: e.pos.z + e.velocity.z / speed * PROJECTILE_LOOKAHEAD,
            };
            boxes.push(Aabb {
                min: Vec3 {
                    x: e.pos.x.min(end.x) - PROJECTILE_MARGIN,
                    y: e.pos.y.min(end.y) - 1.0,
                    z: e.pos.z.min(end.z) - PROJECTILE_MARGIN,
                },
                max: Vec3 {
                    x: e.pos.x.max(end.x) + PROJECTILE_MARGIN,
                    y: e.pos.y.max(end.y) + 2.0,
                    z: e.pos.z.max(end.z) + PROJECTILE_MARGIN,
                },
            });
        }
    }
    boxes
}

impl Params {
    pub fn with_danger(mut self, nearby: &[Nearby]) -> Self {
        self.avoid.extend(danger_boxes(nearby));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3 { x, y, z }
    }

    fn mob(kind: &str, pos: Vec3) -> Nearby {
        Nearby {
            kind: kind.into(),
            pos,
            velocity: v(0.0, 0.0, 0.0),
        }
    }

    #[test]
    fn hostile_mobs_are_boxed_and_passive_ones_are_not() {
        let boxes = danger_boxes(&[
            mob("minecraft:zombie", v(10.5, 1.0, 10.5)),
            mob("minecraft:cow", v(0.5, 1.0, 0.5)),
        ]);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].min.x, 7.5);
        assert_eq!(boxes[0].max.z, 13.5);
    }

    #[test]
    fn a_flying_arrow_blocks_its_line_but_a_stuck_one_does_not() {
        let flying = Nearby {
            kind: "minecraft:arrow".into(),
            pos: v(0.5, 2.0, 0.5),
            velocity: v(1.0, 0.0, 0.0),
        };
        let stuck = mob("minecraft:arrow", v(0.5, 2.0, 0.5));
        let boxes = danger_boxes(&[flying, stuck]);
        assert_eq!(boxes.len(), 1);
        assert!(boxes[0].max.x > 12.0 && boxes[0].min.x < 0.0);
    }

    #[test]
    fn params_with_danger_avoid_the_mob_cell() {
        let p = Params::default().with_danger(&[mob("zombie", v(5.5, 1.0, 5.5))]);
        assert!(p.avoids((5, 1, 5)));
        assert!(p.avoids((7, 1, 5)));
        assert!(!p.avoids((12, 1, 5)));
    }
}
