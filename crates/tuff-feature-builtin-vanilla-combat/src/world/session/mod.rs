pub mod attack;
pub mod enchant;
mod kinetic;

/// Entities a held kinetic weapon touched this charge, with the tick.
#[derive(Default)]
pub struct Stabbed(pub Vec<(i32, u64)>);
