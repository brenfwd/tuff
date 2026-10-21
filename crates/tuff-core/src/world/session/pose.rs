/// Where the player is and how its body is held.
#[derive(Debug, Clone)]
pub struct Pose {
    pub position: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub sneaking: bool,
    pub sprinting: bool,
    /// Sprinting through water, which lays the player flat.
    pub swimming: bool,
}

impl Default for Pose {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
            sneaking: false,
            sprinting: false,
            swimming: false,
        }
    }
}
