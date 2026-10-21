//! The built-in `minecraft:dimension_type` entries, limited to the fields that
//! shape world storage and lighting.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DimensionType {
    pub name: &'static str,
    pub min_y: i32,
    pub height: u32,
    pub logical_height: u32,
    pub has_skylight: bool,
    pub has_ceiling: bool,
    /// The dimension hosts an ender dragon fight.
    pub has_ender_dragon_fight: bool,
    pub coordinate_scale: f64,
    pub ambient_light: f32,
    /// Block tag reference, e.g. `#minecraft:infiniburn_overworld`.
    pub infiniburn: &'static str,
    /// The `minecraft:world_clock` that `/time` uses by default here.
    pub default_clock: Option<&'static str>,
    /// Monsters never spawn where block light is above this (15: no
    /// limit).
    pub monster_spawn_block_light_limit: u8,
    /// The inclusive range the light level monsters spawn at or below is
    /// drawn from.
    pub monster_spawn_light_level: (u8, u8),
}

impl DimensionType {
    pub fn get(name: &str) -> Option<&'static DimensionType> {
        ALL.iter().find(|dimension| dimension.name == name)
    }
}

include!(concat!(env!("OUT_DIR"), "/dimension_types.rs"));
