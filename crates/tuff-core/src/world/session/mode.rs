use super::*;

pub const DEFAULT_FLY_SPEED: f32 = 0.05;

/// Game mode, abilities, permission level and spawn point.
pub struct Mode {
    pub game_mode: GameMode,
    pub previous_game_mode: Option<GameMode>,
    /// The hardcore flag the client last saw in a Login packet.
    pub client_hardcore: bool,
    pub abilities: Abilities,
    /// The flying speed the client was told in its abilities packet.
    pub fly_speed: f32,
    pub permission: PermissionLevel,
    /// The player's own spawn point, if set.
    pub respawn: Option<RespawnSave>,
    /// The entity a spectator watches through, if not itself.
    pub camera: Option<i32>,
    /// The post-processing effects `/posteffect` applied, by id.
    pub post_effects: Vec<String>,
    /// Dimension key and block of the last death, for recovery compasses.
    pub last_death: Option<(String, BlockPos)>,
}
