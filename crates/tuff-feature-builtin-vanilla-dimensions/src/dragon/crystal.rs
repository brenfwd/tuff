//! End crystals: the dragon heals from the nearest one, and any hit
//! destroys one. A hit from anything but an explosion sets off a blast of
//! power 6; the fight is told either way, so the dragon can react.

use crate::dragon::fight::FightsExt;
use rand::Rng;
use tuff_vanilla_data::damage_type::{self, DamageType};
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, BlockState, Item};

use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{
    AttackOutcome, Effect, EntityAttack, EntityBase, EntityHit, RegionEntity, Tick, TickContext,
    take, take_bool,
};
use tuff_core::world::explosion::{BlockInteraction, Explosion};
use tuff_core::world::messages::Igniter;
use tuff_core::world::region_api::RegionWorld;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

use super::fight::FightMessage;

const DATA_BEAM_TARGET: u8 = 8;
const DATA_SHOW_BOTTOM: u8 = 9;
pub const EXPLOSION_POWER: f32 = 6.0;

/// Who destroyed a crystal, if a player did.
type Destroyer = Option<(i32, u128)>;

#[derive(Debug)]
enum Fate {
    Intact,
    /// Hit: it goes off next tick.
    Explode(Explosion, Destroyer),
    /// Caught in an explosion: it vanishes next tick without one.
    Vanish,
}

#[derive(Debug)]
pub struct EndCrystal {
    pub base: EntityBase,
    pub beam_target: Option<BlockPos>,
    pub show_bottom: bool,
    time: i32,
    fate: Fate,
    changed: Vec<u8>,
}

impl EndCrystal {
    pub fn new(id: i32, uuid: u128, position: [f64; 3], show_bottom: bool) -> Self {
        Self {
            base: EntityBase::new(EntityType::EndCrystal, id, uuid, position, [0.0; 3]),
            beam_target: None,
            show_bottom,
            time: 0,
            fate: Fate::Intact,
            changed: Vec::new(),
        }
    }

    pub fn set_beam_target(&mut self, target: Option<BlockPos>) {
        if self.beam_target != target {
            self.beam_target = target;
            self.changed.push(DATA_BEAM_TARGET);
        }
    }

    fn block_pos(&self) -> BlockPos {
        let [x, y, z] = self.base.position;
        BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
    }

    fn value(&self, index: u8) -> Option<MetadataValue> {
        Some(match index {
            DATA_BEAM_TARGET => MetadataValue::OptionalPosition(self.beam_target),
            DATA_SHOW_BOTTOM => MetadataValue::Boolean(self.show_bottom),
            _ => return None,
        })
    }

    /// Destroyed by a hit from anything but an explosion.
    fn shatter(&mut self, player: Option<(i32, u128, String)>, cx: &TickContext) {
        if !matches!(self.fate, Fate::Intact) {
            return;
        }
        let igniter = player.as_ref().map(|(id, uuid, name)| Igniter {
            id: *id,
            uuid: *uuid,
            name: name.clone(),
        });
        let damage_type = if igniter.is_some() {
            &damage_type::PLAYER_EXPLOSION
        } else {
            &damage_type::EXPLOSION
        };
        let blocks = if cx.shared.rule_bool("block_explosion_drop_decay") {
            BlockInteraction::DestroyWithDecay
        } else {
            BlockInteraction::Destroy
        };
        let explosion = Explosion {
            center: self.base.position,
            radius: EXPLOSION_POWER,
            fire: false,
            blocks,
            damage_type,
            igniter,
            culprit: None,
            dangerous_wither_skull: false,
            spares_nether_portals: false,
        };
        self.fate = Fate::Explode(explosion, player.map(|(id, uuid, _)| (id, uuid)));
    }

    /// Crystals do not burn.
    fn immune(&self, damage_type: &DamageType) -> bool {
        self.base.kind.size().fire_immune
            && tuff_core::world::damage_type_in_tag(damage_type, "minecraft:is_fire")
    }

    fn tell_fight(&self, player: Destroyer, cx: &TickContext) {
        if let Some(fight) = cx.shared.fight(cx.dimension) {
            fight.send(FightMessage::CrystalDestroyed {
                uuid: self.base.uuid,
                pos: self.block_pos(),
                player,
            });
        }
    }
}

impl RegionEntity for EndCrystal {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        if let Some(pos) = self.beam_target {
            out.insert("beam_target", Tag::IntArray(vec![pos.x, pos.y, pos.z]));
        }
        out.insert("ShowBottom", self.show_bottom);
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        let mut values = Vec::new();
        if self.beam_target.is_some() {
            values.push((
                DATA_BEAM_TARGET,
                MetadataValue::OptionalPosition(self.beam_target),
            ));
        }
        if !self.show_bottom {
            values.push((DATA_SHOW_BOTTOM, MetadataValue::Boolean(false)));
        }
        values
    }

    fn take_changed_data(&mut self) -> Vec<(u8, MetadataValue)> {
        let mut changed = std::mem::take(&mut self.changed);
        changed.dedup();
        changed
            .into_iter()
            .filter_map(|i| Some((i, self.value(i)?)))
            .collect()
    }

    fn resend_data(&mut self, index: u8) {
        self.changed.push(index);
    }

    fn pickable(&self) -> bool {
        true
    }

    fn hurt(&mut self, _amount: f32, damage_type: &'static DamageType) -> bool {
        if !matches!(self.fate, Fate::Intact) || self.immune(damage_type) {
            return false;
        }
        if tuff_core::world::damage_type_in_tag(damage_type, "minecraft:is_explosion") {
            self.fate = Fate::Vanish;
        } else {
            self.fate = Fate::Explode(
                Explosion {
                    center: self.base.position,
                    radius: EXPLOSION_POWER,
                    fire: false,
                    blocks: BlockInteraction::DestroyWithDecay,
                    damage_type: &damage_type::EXPLOSION,
                    igniter: None,
                    culprit: None,
                    dangerous_wither_skull: false,
                    spares_nether_portals: false,
                },
                None,
            );
        }
        false
    }

    fn attacked(&mut self, attack: &EntityAttack, cx: &mut TickContext) -> Option<AttackOutcome> {
        let name = cx
            .players
            .iter()
            .find(|p| p.id == attack.attacker)
            .map_or_else(String::new, |p| p.name.to_owned());
        self.shatter(Some((attack.attacker, attack.attacker_uuid, name)), cx);
        Some(AttackOutcome {
            hurt: true,
            health_lost: 0.0,
        })
    }

    fn hit(&mut self, hit: &EntityHit, cx: &mut TickContext) -> Option<AttackOutcome> {
        let attacker = hit.attacker.as_ref();
        if attacker.is_some_and(|a| a.kind == EntityType::EnderDragon) || self.immune(hit.kind) {
            return None;
        }
        if tuff_core::world::damage_type_in_tag(hit.kind, "minecraft:is_explosion") {
            self.hurt(hit.amount, hit.kind);
            return Some(AttackOutcome {
                hurt: true,
                health_lost: 0.0,
            });
        }
        let player = attacker
            .filter(|a| a.is_player())
            .map(|a| (a.id, a.uuid, a.name.plain()));
        self.shatter(player, cx);
        Some(AttackOutcome {
            hurt: true,
            health_lost: 0.0,
        })
    }

    fn kill(&mut self, cx: &mut TickContext) -> bool {
        self.tell_fight(None, cx);
        true
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        match std::mem::replace(&mut self.fate, Fate::Intact) {
            Fate::Explode(explosion, player) => {
                self.tell_fight(player, cx);
                return Tick::Explode(explosion);
            }
            Fate::Vanish => {
                self.tell_fight(None, cx);
                return Tick::Discard;
            }
            Fate::Intact => {}
        }
        tuff_core::world::block_contact::pricked_by_cactus(self, cx.block);
        self.time = self.time.wrapping_add(1);
        if cx.shared.fight(cx.dimension).is_some() {
            let pos = self.block_pos();
            if (cx.block)(pos).is_some_and(tuff_world_model::chunk::is_air) {
                cx.effects.push(Effect::SetBlock {
                    pos,
                    state: fire_for(pos, cx),
                });
            }
        }
        Tick::Alive
    }
}

/// The fire that burns at `pos`: soul fire on soul fire bases.
fn fire_for(pos: BlockPos, cx: &TickContext) -> tuff_vanilla_data::BlockState {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let soul = (cx.block)(below).is_some_and(|s| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::SOUL_FIRE_BASE_BLOCKS,
            s.block(),
        )
    });
    if soul {
        Block::SoulFire.default_state()
    } else {
        Block::Fire.default_state()
    }
}

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let beam_target = take(nbt, "beam_target", |tag| match tag {
        Tag::IntArray(v) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
        _ => None,
    });
    let show_bottom = take_bool(nbt, "ShowBottom").unwrap_or(true);
    Some(Box::new(EndCrystal {
        base,
        beam_target,
        show_bottom,
        time: rand::random::<u32>() as i32 % 100_000,
        fate: Fate::Intact,
        changed: Vec::new(),
    }))
}

/// Whether an end crystal may be placed on `state`.
pub fn placeable_on(block: Block) -> bool {
    matches!(block, Block::Obsidian | Block::Bedrock)
}

/// The item that places end crystals.
pub const ITEM: Item = Item::EndCrystal;

/// An end crystal placed on obsidian or bedrock at `pos`, if the
/// block above is air and no entity stands where it would go.
pub fn place(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) -> Option<()> {
    if !placeable_on(state.block()) {
        return None;
    }
    let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
    if !world.owns(above) || !world.block(above).is_some_and(is_air) {
        return None;
    }
    let (x, y, z) = (f64::from(above.x), f64::from(above.y), f64::from(above.z));
    let area = [x, y, z, x + 1.0, y + 2.0, z + 1.0];
    let overlaps = |b: [f64; 6]| {
        area[0] < b[3]
            && area[3] > b[0]
            && area[1] < b[4]
            && area[4] > b[1]
            && area[2] < b[5]
            && area[5] > b[2]
    };
    let players = world.present_players().iter().any(|p| {
        let [px, py, pz] = p.position;
        overlaps([
            px - tuff_core::world::PLAYER_HALF_WIDTH,
            py,
            pz - tuff_core::world::PLAYER_HALF_WIDTH,
            px + tuff_core::world::PLAYER_HALF_WIDTH,
            py + tuff_core::world::PLAYER_HEIGHT,
            pz + tuff_core::world::PLAYER_HALF_WIDTH,
        ])
    });
    let entities = world
        .entity_ids()
        .into_iter()
        .filter_map(|id| world.entity(id))
        .any(|t| overlaps(t.entity.bounds()));
    if players || entities {
        return None;
    }
    let id = world.shared().next_entity_id();
    let crystal = EndCrystal::new(id, world.rng().r#gen(), [x + 0.5, y, z + 0.5], false);
    world.add_entity(Box::new(crystal));
    if let Some(fight) = world.shared().fight(world.dimension_id()) {
        fight.send(FightMessage::TryRespawn);
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_common::end_spikes::{spike_crystal, spikes_for_seed};

    #[tuff_core::watchdog]
    #[test]
    fn spike_crystals_load_standing_on_their_bedrock() {
        let mut registry = tuff_core::world::entity::Registry::default();
        crate::dragon::register(&mut registry);
        for spike in spikes_for_seed(1) {
            let nbt = spike_crystal(&spike, 90.0, false, None);
            let entity = registry.load(&nbt, 1).expect("crystals load");
            let crystal = entity.downcast_ref::<EndCrystal>().expect("an end crystal");
            assert!(crystal.show_bottom);
            assert_eq!(crystal.beam_target, None);
            assert_eq!(crystal.block_pos(), spike.crystal());
        }
        let guarded = spike_crystal(&spikes_for_seed(1)[0], 0.0, true, Some([0, 128, 0]));
        let entity = registry.load(&guarded, 2).unwrap();
        let crystal = entity.downcast_ref::<EndCrystal>().unwrap();
        assert!(crystal.base.invulnerable);
        assert_eq!(crystal.beam_target, Some(BlockPos::new(0, 128, 0)));
        let saved = tuff_core::world::entity::save(entity.as_ref());
        assert_eq!(
            saved.get("beam_target"),
            Some(&Tag::IntArray(vec![0, 128, 0]))
        );
    }
}
