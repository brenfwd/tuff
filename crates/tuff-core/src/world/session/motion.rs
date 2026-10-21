use super::*;

/// Velocity given to the player and the boosts and glides that move it.
#[derive(Default)]
pub struct Motion {
    pub boost_hand: Option<bool>,
    /// The tick a hit, blast or push last gave this player velocity.
    pub pushed_at: Option<u64>,
    /// Velocities sent to this player's own client, newest last.
    pub velocity_sent: VelocitySent,
    pub shoves: ShoveLog,
    pub piston_at: Option<u64>,
    /// Elytra flight: whether this player is gliding and its durability tick.
    pub glide: player::Glide,
    /// The velocity `glide_step`/`firework_boost` track while gliding, kept
    /// only to give a firework's boost a starting point and to nudge the
    /// client's own physics with `SetEntityMotion`.
    pub glide_velocity: [f64; 3],
    /// Horizontal distance the last move covered while gliding.
    pub glide_speed: f64,
    /// Ticks left of an in-progress firework boost.
    pub firework_boost_ticks: u32,
    /// Ticks left spinning from a Riptide launch.
    pub spin_attack_ticks: u32,
}
