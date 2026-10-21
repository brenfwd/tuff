//! A summoned entity of a type no feature implements: it keeps its saved
//! data and shows to clients, but does nothing.

use tuff_vanilla_data::registry::EntityType;

use crate::world::entity::{EntityBase, RegionEntity, Tick, TickContext};
use tuff_nbt::Compound;

#[derive(Debug)]
pub struct Inert {
    base: EntityBase,
}

impl Inert {
    /// Loads `nbt` (without `id`) as `kind`, unless `kind` cannot be summoned.
    pub fn load(kind: EntityType, id: i32, nbt: &Compound) -> Option<Self> {
        if matches!(kind, EntityType::Player | EntityType::FishingBobber) {
            return None;
        }
        let mut rest = nbt.clone();
        rest.remove("id");
        let mut base = EntityBase::load(kind, id, &mut rest)?;
        base.extra = rest;
        Some(Self { base })
    }
}

impl RegionEntity for Inert {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, _out: &mut Compound) {}

    fn tick(&mut self, _context: &mut TickContext) -> Tick {
        Tick::Alive
    }

    fn spawn_data(&self) -> i32 {
        match self.base.kind {
            EntityType::Painting => painting_direction(&self.base.extra),
            _ => 0,
        }
    }
}

const SOUTH: i32 = 3;

fn painting_direction(extra: &Compound) -> i32 {
    match extra.get_byte("facing") {
        Some(0) | None => SOUTH,
        Some(1) => 4,
        Some(2) => 2,
        Some(3) => 5,
        Some(_) => SOUTH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_nbt::Tag;

    fn at_origin() -> Compound {
        Compound::new().with("Pos", Tag::List([0.0f64; 3].map(Tag::Double).to_vec()))
    }

    #[tuff_core::watchdog]
    #[test]
    fn painting_spawns_with_a_horizontal_direction() {
        let summoned = Inert::load(EntityType::Painting, 1, &at_origin()).unwrap();
        assert_eq!(summoned.spawn_data(), SOUTH);
        let east = at_origin().with("facing", 3i8);
        let east = Inert::load(EntityType::Painting, 1, &east).unwrap();
        assert_eq!(east.spawn_data(), 5);
    }
}
