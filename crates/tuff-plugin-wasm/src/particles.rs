//! Particle effects plugins queue with typed options, sent to nearby
//! players after the call the way `/particle` sends them.

use std::sync::Arc;

use tokio::sync::mpsc;
use tuff_core::world::Shared;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::messages::{Directive, DirectiveSource, PlayerEvent};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::ParticleType;

use crate::wit::bindings::types::{ParticleOptions, ParticleSpawn as WitSpawn};

/// One queued particle effect, its options already in packet form.
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleSpawn {
    pub dimension: DimensionId,
    pub particle: u32,
    pub options: Arc<[u8]>,
    pub force: bool,
    pub position: [f64; 3],
    pub delta: [f32; 3],
    pub speed: f32,
    pub count: i32,
    /// Only this player sees it.
    pub viewer: Option<u128>,
}

fn color(rgb: u32) -> Tag {
    Tag::Int(rgb as i32)
}

/// The options compound `/particle` would read for `options`.
pub fn options_compound(options: &ParticleOptions) -> Compound {
    let mut c = Compound::new();
    match options {
        ParticleOptions::None => {}
        ParticleOptions::Block(state) => {
            c.insert("block_state", Tag::String(state.clone()));
        }
        ParticleOptions::Color(argb) => {
            c.insert("color", color(*argb));
        }
        ParticleOptions::Dust(dust) => {
            c.insert("color", color(dust.color));
            c.insert("scale", Tag::Float(dust.scale));
        }
        ParticleOptions::DustTransition(dust) => {
            c.insert("from_color", color(dust.from));
            c.insert("to_color", color(dust.to));
            c.insert("scale", Tag::Float(dust.scale));
        }
        ParticleOptions::Item(id) => {
            let mut item = Compound::new();
            item.insert("id", Tag::String(id.clone()));
            item.insert("count", Tag::Int(1));
            c.insert("item", Tag::Compound(item));
        }
        ParticleOptions::Vibration(vibration) => {
            let pos = vibration.destination;
            let mut destination = Compound::new();
            destination.insert("type", Tag::String("minecraft:block".into()));
            destination.insert(
                "pos",
                Tag::List(vec![Tag::Int(pos.x), Tag::Int(pos.y), Tag::Int(pos.z)]),
            );
            c.insert("destination", Tag::Compound(destination));
            c.insert("arrival_in_ticks", Tag::Int(vibration.arrival_ticks));
        }
        ParticleOptions::Shriek(delay) => {
            c.insert("delay", Tag::Int(*delay));
        }
        ParticleOptions::SculkCharge(roll) => {
            c.insert("roll", Tag::Float(*roll));
        }
        ParticleOptions::Trail(trail) => {
            let t = trail.target;
            c.insert(
                "target",
                Tag::List(vec![Tag::Double(t.x), Tag::Double(t.y), Tag::Double(t.z)]),
            );
            c.insert("color", color(trail.color));
            c.insert("duration", Tag::Int(trail.duration));
        }
        ParticleOptions::Geyser(depth) => {
            c.insert("water_blocks", Tag::Int(*depth));
        }
        ParticleOptions::GeyserBurst(burst) => {
            c.insert("water_blocks", Tag::Int(burst.water_blocks));
            c.insert("burst_impulse_base", Tag::Float(burst.burst_impulse_base));
        }
        ParticleOptions::DragonBreath(power) => {
            c.insert("power", Tag::Float(*power));
        }
        ParticleOptions::Spell(spell) => {
            if let Some(rgb) = spell.color {
                c.insert("color", color(rgb));
            }
            c.insert("power", Tag::Float(spell.power));
        }
    }
    c
}

/// Checks a plugin's spawn and turns it into packet form; `None` when the
/// particle is unknown, its options are rejected or a number is not
/// finite.
pub fn resolve(
    dimension: DimensionId,
    spawn: &WitSpawn,
    viewer: Option<u128>,
) -> Option<ParticleSpawn> {
    let kind = if spawn.particle.contains(':') {
        ParticleType::from_name(&spawn.particle)
    } else {
        ParticleType::from_name(&format!("minecraft:{}", spawn.particle))
    }?;
    let numbers = [
        spawn.pos.x,
        spawn.pos.y,
        spawn.pos.z,
        spawn.delta.x,
        spawn.delta.y,
        spawn.delta.z,
        f64::from(spawn.speed),
    ];
    if numbers.iter().any(|n| !n.is_finite()) || spawn.count < 0 {
        return None;
    }
    let options =
        tuff_core::command::particle::encode_options(kind, &options_compound(&spawn.options))
            .ok()?;
    Some(ParticleSpawn {
        dimension,
        particle: kind.id(),
        options: options.into(),
        force: spawn.force,
        position: [spawn.pos.x, spawn.pos.y, spawn.pos.z],
        delta: [
            spawn.delta.x as f32,
            spawn.delta.y as f32,
            spawn.delta.z as f32,
        ],
        speed: spawn.speed,
        count: spawn.count,
        viewer,
    })
}

/// Sends each queued effect to the players who can see it; each session
/// applies the 32 (or forced 512) block range.
pub async fn apply(shared: &Arc<Shared>, spawns: Vec<ParticleSpawn>, plugin: &str) {
    if spawns.is_empty() {
        return;
    }
    let players = crate::player_state::snapshots(shared).await;
    let source = DirectiveSource {
        name: TextComponent::new(plugin.to_owned()),
        player: None,
    };
    let (reply, _replies) = mpsc::unbounded_channel();
    for spawn in spawns {
        for player in &players {
            if player.state.dimension != spawn.dimension
                || spawn.viewer.is_some_and(|v| v != player.uuid)
            {
                continue;
            }
            player.handle.send(PlayerEvent::Directive {
                directive: Directive::Particle {
                    dimension: spawn.dimension,
                    particle: spawn.particle,
                    options: spawn.options.clone(),
                    force: spawn.force,
                    position: spawn.position,
                    delta: spawn.delta,
                    speed: spawn.speed,
                    count: spawn.count,
                },
                source: source.clone(),
                reply: reply.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wit::bindings::types::{
        BlockPos, DustOptions, DustTransitionOptions, SpellOptions, TrailOptions, Vec3,
        VibrationOptions,
    };

    fn spawn(particle: &str, options: ParticleOptions) -> WitSpawn {
        let zero = Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        WitSpawn {
            particle: particle.into(),
            options,
            pos: Vec3 {
                x: 1.0,
                y: 64.0,
                z: -2.0,
            },
            delta: zero,
            speed: 0.5,
            count: 10,
            force: true,
        }
    }

    fn encoded(particle: &str, options: ParticleOptions) -> Option<Vec<u8>> {
        resolve(DimensionId::default(), &spawn(particle, options), None).map(|s| s.options.to_vec())
    }

    #[tuff_core::watchdog]
    #[test]
    fn typed_options_encode_like_the_particle_command() {
        assert_eq!(encoded("flame", ParticleOptions::None), Some(Vec::new()));
        let dust = encoded(
            "minecraft:dust",
            ParticleOptions::Dust(DustOptions {
                color: 0xff0000,
                scale: 1.0,
            }),
        )
        .unwrap();
        assert_eq!(dust, [0x00, 0xff, 0x00, 0x00, 0x3f, 0x80, 0x00, 0x00]);
        let transition = encoded(
            "dust_color_transition",
            ParticleOptions::DustTransition(DustTransitionOptions {
                from: 1,
                to: 2,
                scale: 2.0,
            }),
        )
        .unwrap();
        assert_eq!(transition.len(), 12);
        assert_eq!(
            encoded("entity_effect", ParticleOptions::Color(0x80ff0000)),
            Some(vec![0x80, 0xff, 0x00, 0x00])
        );
        assert_eq!(encoded("shriek", ParticleOptions::Shriek(5)), Some(vec![5]));
        assert!(encoded("block", ParticleOptions::Block("minecraft:stone".into())).is_some());
        assert!(encoded("item", ParticleOptions::Item("minecraft:apple".into())).is_some());
        assert!(
            encoded(
                "vibration",
                ParticleOptions::Vibration(VibrationOptions {
                    destination: BlockPos { x: 1, y: 2, z: 3 },
                    arrival_ticks: 20,
                }),
            )
            .is_some()
        );
        assert!(
            encoded(
                "trail",
                ParticleOptions::Trail(TrailOptions {
                    target: Vec3 {
                        x: 1.0,
                        y: 2.0,
                        z: 3.0
                    },
                    color: 0xffffff,
                    duration: 10,
                }),
            )
            .is_some()
        );
        assert!(
            encoded(
                "effect",
                ParticleOptions::Spell(SpellOptions {
                    color: None,
                    power: 1.0
                }),
            )
            .is_some()
        );
        assert_eq!(
            encoded("sculk_charge", ParticleOptions::SculkCharge(0.0)),
            Some(vec![0, 0, 0, 0])
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn rejected_spawns_resolve_to_nothing() {
        assert!(encoded("minecraft:nope", ParticleOptions::None).is_none());
        assert!(encoded("dust", ParticleOptions::None).is_none());
        assert!(
            encoded(
                "dust",
                ParticleOptions::Dust(DustOptions {
                    color: 0,
                    scale: 5.0
                })
            )
            .is_none()
        );
        assert!(encoded("block", ParticleOptions::Block("minecraft:nope".into())).is_none());
        assert!(encoded("geyser", ParticleOptions::Geyser(0)).is_none());
        let mut bad = spawn("flame", ParticleOptions::None);
        bad.pos.x = f64::NAN;
        assert!(resolve(DimensionId::default(), &bad, None).is_none());
    }
}
