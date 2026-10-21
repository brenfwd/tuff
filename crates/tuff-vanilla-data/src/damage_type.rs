//! The built-in `minecraft:damage_type` entries.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageType {
    pub name: &'static str,
    /// Selects the death/obituary message's translation key.
    pub message_id: &'static str,
    /// Hunger exhaustion added per point of damage dealt.
    pub exhaustion: f32,
    pub scaling: Scaling,
    pub death_message_type: Option<DeathMessageType>,
    pub effects: Option<DamageEffects>,
}

include!(concat!(env!("OUT_DIR"), "/damage_types.rs"));
