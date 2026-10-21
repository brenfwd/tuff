// SPDX-License-Identifier: MIT
//! A bot's view of the world: block classes read per 16^3 section into a
//! bounded cache, free of host calls behind [`SectionSource`].

mod cache;
pub mod host;
#[cfg(test)]
pub(crate) mod tests;

pub use cache::{BlockCache, SECTION_VOLUME, SectionKey, SectionSource, States};

use std::rc::Rc;

/// What a bot needs to know about a block, as bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Class(u8);

impl Class {
    /// A wall: not passable, not standable. Unloaded and out-of-world blocks.
    pub const NONE: Class = Class(0);
    pub const PASSABLE: Class = Class(1);
    pub const STANDABLE: Class = Class(2);
    pub const CLIMBABLE: Class = Class(4);
    pub const WATER: Class = Class(8);
    pub const DANGER: Class = Class(16);
    /// Sand, gravel and the like: falls when what holds it is removed.
    pub const FALLING: Class = Class(32);
    /// Water or lava: flows into a cell that is opened next to it.
    pub const FLUID: Class = Class(64);

    pub const fn with(self, other: Class) -> Class {
        Class(self.0 | other.0)
    }

    pub const fn has(self, other: Class) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn passable(self) -> bool {
        self.has(Class::PASSABLE)
    }

    pub const fn standable(self) -> bool {
        self.has(Class::STANDABLE)
    }

    pub const fn climbable(self) -> bool {
        self.has(Class::CLIMBABLE)
    }

    pub const fn water(self) -> bool {
        self.has(Class::WATER)
    }

    pub const fn danger(self) -> bool {
        self.has(Class::DANGER)
    }

    pub const fn falling(self) -> bool {
        self.has(Class::FALLING)
    }

    pub const fn fluid(self) -> bool {
        self.has(Class::FLUID)
    }
}

/// The registry traits [`classify`] reads, as the host reports them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Traits {
    pub solid: bool,
    pub air: bool,
    pub liquid: bool,
    pub replaceable: bool,
}

/// The block's name without namespace or properties.
pub fn name_of(state: &str) -> &str {
    let name = state.split('[').next().unwrap_or(state);
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn is_climbable(name: &str) -> bool {
    matches!(
        name,
        "ladder"
            | "vine"
            | "scaffolding"
            | "twisting_vines"
            | "twisting_vines_plant"
            | "weeping_vines"
            | "weeping_vines_plant"
            | "cave_vines"
            | "cave_vines_plant"
    )
}

fn is_hazard_cell(name: &str) -> bool {
    matches!(
        name,
        "fire" | "soul_fire" | "sweet_berry_bush" | "powder_snow" | "cobweb" | "wither_rose"
    )
}

fn is_hazard_solid(name: &str) -> bool {
    matches!(
        name,
        "magma_block" | "cactus" | "campfire" | "soul_campfire"
    )
}

fn is_falling(name: &str) -> bool {
    matches!(
        name,
        "sand"
            | "red_sand"
            | "gravel"
            | "suspicious_sand"
            | "suspicious_gravel"
            | "anvil"
            | "chipped_anvil"
            | "damaged_anvil"
            | "dragon_egg"
    ) || name.ends_with("_concrete_powder")
}

fn is_tall_solid(name: &str) -> bool {
    name.ends_with("_fence")
        || name.ends_with("_fence_gate")
        || name.ends_with("_wall")
        || name == "fence"
}

/// Classifies a block state from its name and registry traits. Danger
/// blocks are never passable or standable; walls are neither; fluids are
/// passable, and only water is swimmable.
pub fn classify(state: &str, traits: Traits) -> Class {
    let name = name_of(state);
    if name == "lava" {
        return Class::DANGER.with(Class::FLUID);
    }
    if name == "water" || name == "bubble_column" {
        return Class::PASSABLE.with(Class::WATER).with(Class::FLUID);
    }
    if is_hazard_cell(name) || is_hazard_solid(name) {
        return Class::DANGER;
    }
    if is_climbable(name) {
        return Class::PASSABLE.with(Class::CLIMBABLE);
    }
    if traits.air {
        return Class::PASSABLE;
    }
    if traits.liquid {
        return Class::DANGER.with(Class::FLUID);
    }
    if is_tall_solid(name) {
        return Class::NONE;
    }
    if traits.solid && is_falling(name) {
        Class::STANDABLE.with(Class::FALLING)
    } else if traits.solid {
        Class::STANDABLE
    } else {
        Class::PASSABLE
    }
}

/// A read that is not available yet: the per-tick read budget ran out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending;

/// Block classes by position, as the pathfinder reads them.
pub trait Blocks {
    fn class_at(&mut self, x: i32, y: i32, z: i32) -> Result<Class, Pending>;

    /// The block state at a position, `None` when it is not known.
    fn state_at(&mut self, _x: i32, _y: i32, _z: i32) -> Result<Option<Rc<str>>, Pending> {
        Ok(None)
    }

    /// Forgets what is known about the blocks around these positions so
    /// the next read sees their current state.
    fn refresh(&mut self, _around: &[(i32, i32, i32)]) {}

    /// Positions in one section whose block is one of `wanted` (names
    /// without namespace). Costs a section read from the budget.
    fn find(
        &mut self,
        _section: SectionKey,
        _wanted: &[String],
    ) -> Result<Vec<(i32, i32, i32)>, Pending> {
        Ok(Vec::new())
    }
}
