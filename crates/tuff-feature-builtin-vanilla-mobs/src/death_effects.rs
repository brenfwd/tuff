//! What a mob's active effects do when it is killed and removed: Weaving
//! leaves cobwebs, Oozing leaves slimes.

use super::silverfish::Silverfish;
use super::slime::Cube;
use super::{Mob, MobCore};
use rand::Rng;
use tuff_core::world::entity::{Culprit, Effect, EntityBase, EntitySummary, TickContext};
use tuff_core::world::explosion::{BlockInteraction, Explosion, WindBurst};
use tuff_core::world::region_api::{PlayerEffectEvent, PlayerEffectTrigger};
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::block::{Facing, Support};
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, MobEffect, SoundEvent};
use tuff_vanilla_data::{Block, Item};
use tuff_world_model::BlockPos;

const COBWEB_TRIES: usize = 15;
const OOZED_SLIMES: i32 = 2;

/// Where an effect bearer stands, and which reactive effects it bears.
pub struct Site {
    id: i32,
    position: [f64; 3],
    height: f64,
    yaw: f32,
    pitch: f32,
    bounds: [f64; 6],
    culprit: Culprit,
    player: bool,
    weaving: bool,
    oozing: bool,
    wind_charged: bool,
    infested: bool,
}

impl Site {
    pub fn of_mob(core: &MobCore) -> Self {
        Self {
            id: core.base.id,
            position: core.base.position,
            height: f64::from(core.size.height),
            yaw: core.base.yaw,
            pitch: core.base.pitch,
            bounds: core.bounds(),
            culprit: Culprit::entity(&core.base),
            player: false,
            weaving: core.effects.has(MobEffect::Weaving),
            oozing: core.effects.has(MobEffect::Oozing),
            wind_charged: core.effects.has(MobEffect::WindCharged),
            infested: core.effects.has(MobEffect::Infested),
        }
    }

    pub fn of_player(event: &PlayerEffectEvent) -> Self {
        let [x, y, z] = event.position;
        let half = tuff_core::world::PLAYER_HALF_WIDTH;
        let has = |effect| event.effects.contains(&effect);
        Self {
            id: -1,
            position: event.position,
            height: event.height,
            yaw: event.yaw,
            pitch: event.pitch,
            bounds: [x - half, y, z - half, x + half, y + event.height, z + half],
            culprit: event.culprit.clone(),
            player: true,
            weaving: has(MobEffect::Weaving),
            oozing: has(MobEffect::Oozing),
            wind_charged: has(MobEffect::WindCharged),
            infested: has(MobEffect::Infested),
        }
    }
}

pub fn removed(core: &MobCore, cx: &mut TickContext) {
    removed_at(&Site::of_mob(core), cx);
}

/// A bearer of Weaving, Oozing or Wind Charged was killed.
pub fn removed_at(site: &Site, cx: &mut TickContext) {
    if site.weaving && (site.player || cx.shared.rule_bool("mob_griefing")) {
        cobwebs(site, cx);
    }
    if site.oozing {
        slimes(site, cx);
    }
    if site.wind_charged {
        gust(site, cx);
    }
}

/// A player bearing reactive effects died or was hurt.
pub fn player(event: &PlayerEffectEvent, cx: &mut TickContext) {
    let site = Site::of_player(event);
    match event.trigger {
        PlayerEffectTrigger::Died => removed_at(&site, cx),
        PlayerEffectTrigger::Hurt => hurt_at(&site, cx),
    }
}

/// A mob the Wither killed leaves a wither rose where it died, or the item
/// when the rose cannot be planted.
pub fn wither_rose(core: &MobCore, cx: &mut TickContext) {
    let pos = super::physics::block_pos(core.base.position);
    let level = |p: BlockPos| (cx.block)(p).unwrap_or_else(|| Block::Air.default_state());
    let state = Block::WitherRose.default_state();
    let plants = cx.shared.rule_bool("mob_griefing")
        && matches!(
            level(pos).block(),
            Block::Air | Block::CaveAir | Block::VoidAir
        )
        && tuff_vanilla_common::placement::can_survive(state, pos, &level);
    if plants {
        cx.effects.push(Effect::SetBlock { pos, state });
    } else {
        #[expect(
            clippy::disallowed_methods,
            reason = "item a mob spawns, wears or throws (wither_rose)"
        )]
        core.drop_item(
            ItemStack::mint(Item::WitherRose, 1, MintReason::Spawn),
            0.0,
            cx,
        );
    }
}

const GUST_BASE_POWER: f32 = 3.0;
const GUST_EXTRA_POWER: f32 = 2.0;
const INFESTED_CHANCE: f32 = 0.1;

fn gust(site: &Site, cx: &mut TickContext) {
    let [x, y, z] = site.position;
    let power = GUST_BASE_POWER + cx.rng.r#gen::<f32>() * GUST_EXTRA_POWER;
    cx.effects.push(Effect::Explode(Explosion {
        dangerous_wither_skull: false,
        spares_nether_portals: false,
        center: [x, y + site.height / 2.0, z],
        radius: power,
        fire: false,
        blocks: BlockInteraction::Trigger(WindBurst {
            knockback: 1.0,
            sound: SoundEvent::EntityBreezeWindBurst,
            source: EntityType::BreezeWindCharge,
            spares_fall: false,
        }),
        damage_type: &damage_type::WIND_CHARGE,
        igniter: None,
        culprit: Some(Box::new(site.culprit.clone())),
    }));
}

/// Infested: a hurt mob may release one or two silverfish.
pub fn hurt(core: &MobCore, cx: &mut TickContext) {
    hurt_at(&Site::of_mob(core), cx);
}

fn hurt_at(site: &Site, cx: &mut TickContext) {
    if !site.infested || cx.rng.r#gen::<f32>() > INFESTED_CHANCE {
        return;
    }
    let [x, y, z] = site.position;
    let at = [x, y + site.height / 2.0, z];
    let count = cx.rng.gen_range(1..=2);
    for _ in 0..count {
        let spread = cx
            .rng
            .gen_range(-std::f32::consts::FRAC_PI_2..=std::f32::consts::FRAC_PI_2);
        let look = tuff_core::world::raycast::view_vector(site.yaw, site.pitch);
        let (sin, cos) = spread.sin_cos();
        let (lx, lz) = (look[0] as f32 * 0.3, look[2] as f32 * 0.3);
        let velocity = [
            f64::from(lx * cos + lz * sin),
            look[1] * 0.45,
            f64::from(lz * cos - lx * sin),
        ];
        let mut base = EntityBase::new(
            EntityType::Silverfish,
            cx.shared.next_entity_id(),
            cx.rng.r#gen(),
            at,
            velocity,
        );
        base.yaw = cx.rng.r#gen::<f32>() * 360.0;
        let silverfish = Mob::<Silverfish>::new(base, cx.rng);
        silverfish
            .core
            .sound(SoundEvent::EntitySilverfishHurt, 1.0, 1.0, cx);
        cx.effects.push(Effect::Spawn(Box::new(silverfish)));
    }
}

fn cobwebs(site: &Site, cx: &mut TickContext) {
    let [x, y, z] = site.position;
    let origin = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
    let wanted = cx.rng.gen_range(2..=3);
    let mut chosen: Vec<BlockPos> = Vec::new();
    for _ in 0..COBWEB_TRIES {
        let pos = BlockPos::new(
            origin.x + cx.rng.gen_range(-1..=1),
            origin.y + cx.rng.gen_range(-1..=1),
            origin.z + cx.rng.gen_range(-1..=1),
        );
        if chosen.contains(&pos) {
            continue;
        }
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        let open = (cx.block)(pos).is_some_and(|s| s.is_replaceable());
        let floor = (cx.block)(below).is_some_and(|s| s.is_face_sturdy(Facing::Up, Support::Full));
        if open && floor {
            chosen.push(pos);
            if chosen.len() >= wanted {
                break;
            }
        }
    }
    for pos in chosen {
        cx.effects.push(Effect::SetBlock {
            pos,
            state: Block::Cobweb.default_state(),
        });
    }
}

fn nearby_slimes(site: &Site, near: &[EntitySummary], limit: usize) -> usize {
    let reach = super::physics::inflate(site.bounds, [2.0; 3]);
    near.iter()
        .filter(|e| e.kind == EntityType::Slime && e.id != site.id)
        .filter(|e| super::physics::intersects(e.bounds, reach))
        .take(limit)
        .count()
}

fn slimes(site: &Site, cx: &mut TickContext) {
    let max = cx.shared.rule_int("max_entity_cramming");
    let count = if max < 1 {
        OOZED_SLIMES
    } else {
        let near = nearby_slimes(site, cx.entities, max as usize) as i32;
        (max - near).clamp(0, OOZED_SLIMES)
    };
    let [x, y, z] = site.position;
    for _ in 0..count {
        let slime = Cube::oozed([x, y + 0.5, z], cx);
        cx.effects.push(Effect::Spawn(Box::new(slime)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{with_context, with_rule};

    fn roses(cx: &TickContext) -> (usize, usize) {
        let planted = cx
            .effects
            .iter()
            .filter(|e| matches!(e, Effect::SetBlock { state, .. } if state.block() == Block::WitherRose))
            .count();
        let dropped = cx
            .effects
            .iter()
            .filter(|e| matches!(e, Effect::DropItem { .. }))
            .count();
        (planted, dropped)
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_mob_the_wither_kills_leaves_a_rose_on_grass() {
        with_context(|core, cx| {
            core.base.position = [0.5, 64.0, 0.5];
            wither_rose(core, cx);
            assert_eq!(roses(cx), (1, 0));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_rose_drops_as_an_item_without_mob_griefing() {
        with_rule("mob_griefing", false, |core, cx| {
            core.base.position = [0.5, 64.0, 0.5];
            wither_rose(core, cx);
            assert_eq!(roses(cx), (0, 1));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_rose_drops_as_an_item_where_it_cannot_survive() {
        with_context(|core, cx| {
            core.base.position = [0.5, 70.0, 0.5];
            wither_rose(core, cx);
            assert_eq!(roses(cx), (0, 1));
        });
    }
}
