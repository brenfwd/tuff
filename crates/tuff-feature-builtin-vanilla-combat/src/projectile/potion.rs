//! Thrown splash and lingering potions landing: the splash's effects on
//! everyone near it (scaled by distance), water putting out fires and
//! burning entities, and the lingering cloud.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, Potion};
use tuff_vanilla_data::tag::Key;
use tuff_vanilla_data::{BlockState, tag};

use tuff_core::protocol::play;
use tuff_core::world::effects::{self, ActiveEffect};
use tuff_core::world::entity::{Culprit, Effect, EntityHit, TickContext};
use tuff_core::world::physics::{Aabb, inflate};
use tuff_core::world::{framed, packets};
use tuff_item::item::{ItemStack, PotionContents};
use tuff_vanilla_common::effect_cloud::AreaEffectCloud;
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

const SPLASH_RANGE_SQ: f64 = 16.0;
/// Takes water-contact damage from a splashed water bottle. 26.3's data
/// pack has no tag for this set (it is a fixed list in the game itself),
/// so it is fixed here too.
const WATER_SENSITIVE: [EntityType; 4] = [
    EntityType::Blaze,
    EntityType::SnowGolem,
    EntityType::Strider,
    EntityType::Enderman,
];
/// Splashed effects that would end within this many ticks are dropped.
const MIN_SPLASH_DURATION: i64 = 20;

pub use tuff_core::world::effects::PotionHit;

fn contents_of(item: &ItemStack) -> PotionContents {
    item.components
        .potion_contents
        .as_deref()
        .cloned()
        .unwrap_or_default()
}

/// Whether the potion is exactly the listed potion, without custom
/// effects, as potion tags are tested.
fn is_tagged(contents: &PotionContents, potions: Key<Potion>) -> bool {
    contents.custom_effects.is_empty()
        && contents
            .potion
            .is_some_and(|p| tuff_datapack::tags::is(potions, p))
}

fn distance_sq(a: Aabb, b: Aabb) -> f64 {
    let gap = |min_a: f64, max_a: f64, min_b: f64, max_b: f64| {
        (min_b - max_a).max(min_a - max_b).max(0.0)
    };
    let dx = gap(a[0], a[3], b[0], b[3]);
    let dy = gap(a[1], a[4], b[1], b[4]);
    let dz = gap(a[2], a[5], b[2], b[5]);
    dx * dx + dy * dy + dz * dz
}

/// Whether a target's feet-center is within splash range of where the
/// potion broke, as the fire and water-sensitive checks use it.
fn within_splash_point_range(target_bounds: Aabb, position: [f64; 3]) -> bool {
    let center = [
        (target_bounds[0] + target_bounds[3]) / 2.0,
        target_bounds[1],
        (target_bounds[2] + target_bounds[5]) / 2.0,
    ];
    let d = [
        center[0] - position[0],
        center[1] - position[1],
        center[2] - position[2],
    ];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < SPLASH_RANGE_SQ
}

fn intersects(a: Aabb, b: Aabb) -> bool {
    a[0] < b[3] && a[3] > b[0] && a[1] < b[4] && a[4] > b[1] && a[2] < b[5] && a[5] > b[2]
}

/// The level events a potion shows where it lands: instant potions have
/// their own particles and sound.
fn splash_events(contents: &PotionContents, at: BlockPos) -> Effect {
    let instant = contents
        .potion
        .is_some_and(|potion| potion.effects().iter().any(|e| e.effect.is_instantaneous()));
    let (particles, sound) = if instant {
        (
            play::level_event::PARTICLES_INSTANT_POTION_SPLASH,
            play::level_event::SOUND_INSTANT_POTION_SPLASH,
        )
    } else {
        (
            play::level_event::PARTICLES_SPELL_POTION_SPLASH,
            play::level_event::SOUND_SPELL_POTION_SPLASH,
        )
    };
    let event = |event, data| {
        framed(&play::LevelEvent {
            event,
            position: at,
            data,
            global: false,
        })
    };
    Effect::ChunkPackets(packets([
        event(particles, effects::potion_color(contents)),
        event(sound, 0),
    ]))
}

/// A water potion hitting a block's `face` douses fire and lit campfires
/// and candles around the block in front of it.
pub fn douse(pos: BlockPos, face: Facing, cx: &mut TickContext) {
    let front = step(pos, face);
    let mut spots = vec![front, step(front, face.opposite())];
    for side in Facing::HORIZONTAL {
        spots.push(step(front, side));
    }
    for spot in spots {
        let Some(state) = (cx.block)(spot) else {
            continue;
        };
        if tuff_datapack::tags::is(tag::block::key::FIRE, state.block()) {
            cx.effects.push(Effect::DestroyBlock {
                pos: spot,
                drop: false,
            });
        } else if lit(state)
            && (tuff_datapack::tags::is(tag::block::key::CANDLES, state.block())
                || tuff_datapack::tags::is(tag::block::key::CANDLE_CAKES, state.block())
                || tuff_datapack::tags::is(tag::block::key::CAMPFIRES, state.block()))
            && let Some(out) = state.with("lit", "false")
        {
            if tuff_datapack::tags::is(tag::block::key::CAMPFIRES, state.block()) {
                cx.effects
                    .push(Effect::ChunkPackets(packets([framed(&play::LevelEvent {
                        event: play::level_event::SOUND_EXTINGUISH_FIRE,
                        position: spot,
                        data: 0,
                        global: false,
                    })])));
            }
            cx.effects.push(Effect::SetBlock {
                pos: spot,
                state: out,
            });
        }
    }
}

fn lit(state: BlockState) -> bool {
    state.get("lit") == Some("true")
}

/// A thrown potion of `kind` with `item` landing: `bounds` is its box moved
/// to where it hit, `at` its block, `struck` the entity it hit directly.
#[expect(
    clippy::too_many_arguments,
    reason = "independent inputs of one operation; a parameter struct would be built once"
)]
pub fn land(
    kind: EntityType,
    item: &ItemStack,
    bounds: Aabb,
    at: BlockPos,
    position: [f64; 3],
    struck: Option<[f64; 3]>,
    margin: f64,
    owner: Option<Culprit>,
    cx: &mut TickContext,
) {
    let contents = contents_of(item);
    let area = inflate(bounds, [4.0, 2.0, 4.0]);
    let extinguish = is_tagged(&contents, tag::potion::key::EXTINGUISHES_ENTITIES);
    let all = effects::potion_effects(&contents, 1.0);
    let has_effects = !all.is_empty();
    let mut targets: Vec<(i32, bool, EntityType, Aabb)> = cx
        .players
        .iter()
        .filter(|p| intersects(area, p.bounds()))
        .map(|p| (p.id, true, EntityType::Player, p.bounds()))
        .collect();
    targets.extend(
        cx.entities
            .iter()
            .filter(|e| e.living.is_some_and(|l| l.alive) && intersects(area, e.bounds))
            .map(|e| (e.id, false, e.kind, e.bounds)),
    );
    if kind == EntityType::SplashPotion && has_effects {
        let scale_duration = item.item().potion_duration_scale();
        for (id, player, target_kind, target_bounds) in &targets {
            let grown = inflate(*target_bounds, [margin; 3]);
            let dist = distance_sq(bounds, grown);
            if dist >= SPLASH_RANGE_SQ {
                continue;
            }
            let scale = 1.0 - dist.sqrt() / 4.0;
            let mut timed = Vec::new();
            let mut instant = Vec::new();
            for effect in &all {
                if effect.effect.is_instantaneous() {
                    instant.push((effect.effect, effect.amplifier));
                    continue;
                }
                let mut sized =
                    ActiveEffect::timed(effect.effect, effect.duration, effect.amplifier);
                sized.ambient = effect.ambient;
                sized.show_particles = effect.show_particles;
                sized.show_icon = effect.show_particles;
                if !sized.is_infinite() && sized.duration != 0 {
                    sized.duration = (scale * sized.duration as f64 * f64::from(scale_duration)
                        + 0.5) as i32 as i64;
                }
                if sized.is_infinite() || sized.duration > MIN_SPLASH_DURATION {
                    timed.push(sized);
                }
            }
            effects::apply_potion(
                *id,
                *player,
                *target_kind,
                &timed,
                &instant,
                scale,
                false,
                owner.clone(),
                cx,
            );
        }
    }
    if extinguish {
        for (id, player, target_kind, target_bounds) in &targets {
            if within_splash_point_range(*target_bounds, position) && *player {
                effects::apply_potion(*id, true, *target_kind, &[], &[], 1.0, true, None, cx);
            }
        }
    }
    if is_tagged(&contents, tag::potion::key::HURTS_WATER_SENSITIVE_ENTITIES) {
        for (id, _, target_kind, target_bounds) in &targets {
            if WATER_SENSITIVE.contains(target_kind)
                && within_splash_point_range(*target_bounds, position)
            {
                let mut hurt = EntityHit::new(1.0, &damage_type::INDIRECT_MAGIC);
                hurt.attacker = owner.clone();
                cx.effects.push(Effect::Hurt {
                    target: *id,
                    hit: Box::new(hurt),
                });
            }
        }
    }
    if is_tagged(&contents, tag::potion::key::REHYDRATES_AXOLOTLS) {
        for (id, _, target_kind, _) in &targets {
            if *target_kind == EntityType::Axolotl {
                cx.effects.push(Effect::Notify {
                    target: *id,
                    notice: tuff_core::world::entity::Notice::Rehydrate,
                });
            }
        }
    }
    if kind == EntityType::LingeringPotion && has_effects {
        let cloud = AreaEffectCloud::from_potion(
            0,
            struck.unwrap_or(position),
            owner,
            contents.clone(),
            item.item().potion_duration_scale(),
            cx.rng,
        );
        cx.effects.push(Effect::Spawn(Box::new(cloud)));
    }
    cx.effects.push(splash_events(&contents, at));
}

#[cfg(test)]
mod cloud_tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use tuff_core::world::entity::{EntitySummary, LivingSummary, RegionEntity};

    use super::*;

    fn zombie(bounds_at: [f64; 3]) -> EntitySummary {
        let [x, y, z] = bounds_at;
        EntitySummary {
            id: 40,
            uuid: 40,
            kind: EntityType::Zombie,
            position: bounds_at,
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [x - 0.3, y, z - 0.3, x + 0.3, y + 1.9, z + 0.3],
            living: Some(LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 1.7,
                in_water: false,
                seen_as_enemy: true,
                enemy: true,
                health: 20.0,
                tamed: false,
                owner: None,
                chasing: None,
            }),
            pickable: true,
            item: None,
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lingering_potion_cloud_applies_its_lasting_effect_to_mobs_inside() {
        let mut rng = StdRng::seed_from_u64(1);
        let contents = PotionContents {
            potion: Some(Potion::Poison),
            ..PotionContents::default()
        };
        let mut cloud =
            AreaEffectCloud::from_potion(5, [0.0, 64.0, 0.0], None, contents, 0.25, &mut rng);
        let inside = [zombie([1.0, 64.0, 0.0])];
        let mut applied = 0;
        tuff_test_support::with_tick_context_around(&inside, |cx| {
            for _ in 0..30 {
                cloud.tick(cx);
            }
            applied = cx
                .effects
                .iter()
                .filter(|e| matches!(e, Effect::MobEffects { target: 40, .. }))
                .count();
        });
        assert!(applied >= 1, "the cloud poisons the zombie standing in it");
    }
}
