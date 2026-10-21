//! Reads and queued changes of an online player addressed by uuid, usable
//! outside the callback the player acts in.

use std::sync::Arc;

use tuff_core::protocol::play::GameMode;
use tuff_core::world::Shared;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::messages::{CoordinatorMessage, Directive, PlayerSnapshot};
use tuff_vanilla_data::registry::MobEffect;
use tuff_world_model::BlockPos;

use crate::sandbox::STANDING_EYE_HEIGHT;
use crate::wit::bindings::types::{self as wit, Location, Rotation, Vec3};

/// A change to a player a call queued.
#[derive(Clone, Debug, PartialEq)]
pub enum PlayerOp {
    GameMode(GameMode),
    Vitals {
        health: Option<f32>,
        food: Option<i32>,
    },
    Xp {
        level: i32,
        points: i32,
    },
    EffectGive {
        effect: MobEffect,
        duration: i32,
        amplifier: u8,
        particles: bool,
    },
    EffectClear(Option<MobEffect>),
    Kill,
    Respawn,
    SpawnPoint {
        dimension: DimensionId,
        pos: [i32; 3],
        yaw: f32,
        pitch: f32,
    },
}

impl PlayerOp {
    fn directives(self) -> Vec<Directive> {
        match self {
            Self::GameMode(mode) => vec![Directive::GameMode(mode)],
            Self::Vitals { health, food } => vec![Directive::SetVitals { health, food }],
            Self::Xp { level, points } => vec![
                Directive::Experience {
                    amount: level,
                    levels: true,
                    set: true,
                },
                Directive::Experience {
                    amount: points,
                    levels: false,
                    set: true,
                },
            ],
            Self::EffectGive {
                effect,
                duration,
                amplifier,
                particles,
            } => vec![Directive::EffectGive {
                effect,
                duration,
                amplifier,
                particles,
            }],
            Self::EffectClear(effect) => vec![Directive::EffectClear(effect)],
            Self::Kill => vec![Directive::Kill],
            Self::Respawn => vec![Directive::Respawn],
            Self::SpawnPoint {
                dimension,
                pos: [x, y, z],
                yaw,
                pitch,
            } => vec![Directive::SpawnPoint {
                dimension,
                pos: BlockPos { x, y, z },
                yaw,
                pitch,
            }],
        }
    }
}

/// Carries out the ops a call queued for `(player uuid, op)`, in order.
pub async fn apply(shared: &Arc<Shared>, ops: Vec<(u128, PlayerOp)>) {
    for (uuid, op) in ops {
        for directive in op.directives() {
            let _ = shared
                .coordinator
                .send(CoordinatorMessage::PluginDirective {
                    uuid,
                    directive: Box::new(directive),
                })
                .await;
        }
    }
}

/// Every online player as the coordinator last heard of them.
pub async fn snapshots(shared: &Arc<Shared>) -> Vec<PlayerSnapshot> {
    let (reply, players) = tokio::sync::oneshot::channel();
    let _ = shared
        .coordinator
        .send(CoordinatorMessage::Players { reply })
        .await;
    players.await.unwrap_or_default()
}

pub fn game_mode_of(mode: wit::GameMode) -> GameMode {
    match mode {
        wit::GameMode::Survival => GameMode::Survival,
        wit::GameMode::Creative => GameMode::Creative,
        wit::GameMode::Adventure => GameMode::Adventure,
        wit::GameMode::Spectator => GameMode::Spectator,
    }
}

fn wit_mode(mode: GameMode) -> wit::GameMode {
    match mode {
        GameMode::Survival => wit::GameMode::Survival,
        GameMode::Creative => wit::GameMode::Creative,
        GameMode::Adventure => wit::GameMode::Adventure,
        GameMode::Spectator => wit::GameMode::Spectator,
    }
}

pub fn effect_of(name: &str) -> Option<MobEffect> {
    let qualified = crate::portal::qualified(name);
    MobEffect::from_name(&qualified)
}

pub fn location(shared: &Shared, snapshot: &PlayerSnapshot) -> Location {
    let [x, y, z] = snapshot.state.position;
    let [yaw, pitch] = snapshot.state.rotation;
    Location {
        dimension: shared.dimensions.get(snapshot.state.dimension).key.clone(),
        pos: Vec3 { x, y, z },
        rotation: Some(Rotation { yaw, pitch }),
        eye_height: STANDING_EYE_HEIGHT,
    }
}

pub fn state_of(shared: &Shared, snapshot: &PlayerSnapshot) -> wit::PlayerState {
    let state = &snapshot.state;
    wit::PlayerState {
        location: location(shared, snapshot),
        game_mode: wit_mode(state.game_mode),
        alive: state.alive,
        health: state.health,
        food: state.food,
        xp_level: state.level,
        xp_points: state.experience_total,
        fake: snapshot.fake,
        effects: state
            .effects
            .iter()
            .map(|(effect, amplifier)| wit::ActiveEffect {
                effect: effect.name().to_owned(),
                amplifier: *amplifier,
            })
            .collect(),
    }
}
