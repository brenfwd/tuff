use super::*;

/// Ongoing hand actions: digging, item use, cooldowns and the fishing bobber.
#[derive(Default)]
pub struct Actions {
    /// The tick the attack cooldown last restarted (a swing, an attack, a
    /// different item in the main hand).
    pub attack_reset_tick: u64,
    /// The main-hand item as of the last tick, to restart the cooldown
    /// when it changes.
    pub last_main_hand: Option<Item>,
    pub destroying: Option<Destroying>,
    pub using: Option<UsingItem>,
    /// Cooldown group to the tick it ends.
    pub cooldowns: HashMap<String, u64>,
    pub portal_cooldown: u32,
    pub paying: super::paid_entities::Paying,
}
