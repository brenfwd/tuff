use rand::Rng;

use crate::goal::{Flags, Goal};
use crate::{MobCore, random_pos};
use tuff_core::world::entity::TickContext;

/// Walks back to its home from beyond `close` blocks, but never from
/// beyond `far`.
#[derive(Debug)]
pub struct WalkToHome {
    speed: f64,
    close: f64,
    far: f64,
}

impl WalkToHome {
    const INTERVAL: i32 = 60;

    pub fn new(speed: f64, close: f64, far: f64) -> Self {
        Self { speed, close, far }
    }

    fn home_centre(mob: &MobCore) -> Option<[f64; 3]> {
        mob.home.map(|home| home.pos.center())
    }
}

impl Goal for WalkToHome {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(home) = Self::home_centre(mob) else {
            return false;
        };
        let away = mob.distance_sq(home);
        away > self.close * self.close
            && away <= self.far * self.far
            && cx.rng.gen_range(0..Self::INTERVAL) == 0
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(home) = Self::home_centre(mob) {
            mob.move_to_position(home, self.speed, cx);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}

/// Wanders on land while within `radius` blocks of its home.
#[derive(Debug)]
pub struct StrollAroundHome {
    speed: f64,
    radius: f64,
    target: [f64; 3],
}

impl StrollAroundHome {
    const INTERVAL: i32 = 60;

    pub fn new(speed: f64, radius: f64) -> Self {
        Self {
            speed,
            radius,
            target: [0.0; 3],
        }
    }
}

impl Goal for StrollAroundHome {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(home) = mob.home else {
            return false;
        };
        if mob.distance_sq(home.pos.center()) > self.radius * self.radius
            || cx.rng.gen_range(0..Self::INTERVAL) != 0
        {
            return false;
        }
        match random_pos::land_pos(mob, 8, 6, cx) {
            Some(target) => {
                self.target = target;
                true
            }
            None => false,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.move_to_position(self.target, self.speed, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}
