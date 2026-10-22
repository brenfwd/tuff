//! Mannequin: a player-shaped living entity with no AI. It falls, is
//! knocked back, takes damage and dies like any living entity; `immovable`
//! only zeroes movement input, which a mannequin never has. It is not a
//! mob, so the player avatar's main arm and skin parts sit where mobs keep
//! their flags byte. A profile given by name is resolved by the client.

use rand::rngs::StdRng;
use tuff_vanilla_data::entity::Size;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::{MobCore, MobSounds, Species};
use tuff_core::protocol::play::{MetadataValue, ResolvableProfile, metadata};
use tuff_core::world::entity::{Culprit, Effect, EntityHit, TickContext, take_bool};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_common::combat;
use tuff_vanilla_data::damage_type;

const STRIKE_REACH: f64 = 3.0;
const MAIN_ARM: u8 = 15;
const SKIN_PARTS: u8 = 16;
const PROFILE: u8 = 17;
const IMMOVABLE: u8 = 18;
const DESCRIPTION: u8 = 19;

const DEFAULT_DESCRIPTION: &str = "entity.minecraft.mannequin.label";

const LAYERS: [(&str, i8); 7] = [
    ("cape", 0x01),
    ("jacket", 0x02),
    ("left_sleeve", 0x04),
    ("right_sleeve", 0x08),
    ("left_pants_leg", 0x10),
    ("right_pants_leg", 0x20),
    ("hat", 0x40),
];
const ALL_LAYERS: i8 = 0x7f;

/// The poses a mannequin may be saved in, with their network ids.
const POSES: [(&str, i32); 5] = [
    ("standing", 0),
    ("fall_flying", 1),
    ("sleeping", 2),
    ("swimming", 3),
    ("crouching", 5),
];

#[derive(Debug)]
pub struct Mannequin {
    /// The saved form, written back verbatim.
    profile_nbt: Option<Tag>,
    profile: ResolvableProfile,
    immovable: bool,
    shown_layers: i8,
    left_handed: bool,
    pose: usize,
    description: Option<Tag>,
    hide_description: bool,
    last_strike: Option<u64>,
}

impl Mannequin {
    fn size(&self, core: &MobCore) -> Size {
        let standing = core.base.kind.size();
        let (width, height, eye_height) = match POSES[self.pose].0 {
            "crouching" => (0.6, 1.5, 1.27),
            "swimming" | "fall_flying" => (0.6, 0.6, 0.4),
            "sleeping" => (0.2, 0.2, 0.2),
            _ => return standing,
        };
        Size {
            width,
            height,
            eye_height,
            ..standing
        }
    }

    fn description(&self) -> Option<TextComponent> {
        if self.hide_description {
            return None;
        }
        Some(
            self.description
                .as_ref()
                .and_then(TextComponent::from_nbt)
                .unwrap_or_else(|| TextComponent::translate(DEFAULT_DESCRIPTION, [])),
        )
    }
}

impl Mannequin {
    /// Takes a spawn egg's `minecraft:profile` component; false when it
    /// does not parse.
    pub fn take_profile(&mut self, core: &mut MobCore, tag: &Tag) -> bool {
        let Some(profile) = ResolvableProfile::from_nbt(tag) else {
            return false;
        };
        self.profile = profile;
        self.profile_nbt = Some(tag.clone());
        core.changed(PROFILE);
        true
    }
}

impl Species for Mannequin {
    fn create(_rng: &mut StdRng) -> Self {
        Self {
            profile_nbt: None,
            profile: ResolvableProfile::default(),
            immovable: false,
            shown_layers: ALL_LAYERS,
            left_handed: false,
            pose: 0,
            description: None,
            hide_description: false,
            last_strike: None,
        }
    }

    fn configure(core: &mut MobCore) {
        core.not_a_mob = true;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        if let Some(tag) = nbt.remove("profile") {
            if let Some(profile) = ResolvableProfile::from_nbt(&tag) {
                self.profile = profile;
                self.profile_nbt = Some(tag);
            } else {
                nbt.insert("profile", tag);
            }
        }
        self.immovable = take_bool(nbt, "immovable").unwrap_or(false);
        if let Some(Tag::List(hidden)) = nbt.remove("hidden_layers") {
            self.shown_layers = hidden.iter().fold(ALL_LAYERS, |shown, part| {
                let mask = match part {
                    Tag::String(name) => LAYERS
                        .iter()
                        .find(|(n, _)| n == name)
                        .map_or(0, |(_, m)| *m),
                    _ => 0,
                };
                shown & !mask
            });
        }
        if let Some(Tag::String(hand)) = nbt.get("main_hand")
            && matches!(hand.as_str(), "left" | "right")
        {
            self.left_handed = hand == "left";
            nbt.remove("main_hand");
        }
        if let Some(Tag::String(pose)) = nbt.get("pose")
            && let Some(index) = POSES.iter().position(|(name, _)| name == pose)
        {
            self.pose = index;
            nbt.remove("pose");
        }
        self.hide_description = take_bool(nbt, "hide_description").unwrap_or(false);
        if let Some(tag) = nbt.remove("description") {
            if TextComponent::from_nbt(&tag).is_some() {
                self.description = Some(tag);
            } else {
                nbt.insert("description", tag);
            }
        }
        core.size = self.size(core);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        if let Some(profile) = &self.profile_nbt {
            out.insert("profile", profile.clone());
        }
        let hidden = LAYERS
            .iter()
            .filter(|(_, mask)| self.shown_layers & mask == 0)
            .map(|(name, _)| Tag::String((*name).to_owned()))
            .collect();
        out.insert("hidden_layers", Tag::List(hidden));
        out.insert("main_hand", if self.left_handed { "left" } else { "right" });
        out.insert("pose", POSES[self.pose].0);
        out.insert("immovable", self.immovable);
        if let Some(description) = &self.description {
            out.insert("description", description.clone());
        }
        if self.hide_description {
            out.insert("hide_description", true);
        }
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (metadata::POSE, MetadataValue::Pose(POSES[self.pose].1)),
            (
                MAIN_ARM,
                MetadataValue::HumanoidArm(i32::from(!self.left_handed)),
            ),
            (SKIN_PARTS, MetadataValue::Byte(self.shown_layers)),
            (
                PROFILE,
                MetadataValue::ResolvableProfile(Box::new(self.profile.clone())),
            ),
            (IMMOVABLE, MetadataValue::Boolean(self.immovable)),
            (DESCRIPTION, MetadataValue::OptionalText(self.description())),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: None,
            hurt: Some(SoundEvent::EntityGenericHurt),
            death: Some(SoundEvent::EntityGenericDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }

    fn melee_strike(&mut self, core: &mut MobCore, victim: u128, cx: &mut TickContext) -> bool {
        let (target, kind, bounds) =
            if let Some(player) = cx.players.iter().find(|p| p.uuid == victim) {
                let [x, y, z] = player.position;
                (
                    player.id,
                    EntityType::Player,
                    [x - 0.3, y, z - 0.3, x + 0.3, y + player.height, z + 0.3],
                )
            } else if let Some(entity) = cx.entities.iter().find(|e| e.uuid == victim) {
                (entity.id, entity.kind, entity.bounds)
            } else {
                return false;
            };
        let eye = core.eye_position();
        let reach_sq: f64 = (0..3)
            .map(|i| (eye[i].clamp(bounds[i], bounds[i + 3]) - eye[i]).powi(2))
            .sum();
        if reach_sq > STRIKE_REACH * STRIKE_REACH {
            return false;
        }
        core.swing(cx);
        let worn = core.equipment.worn();
        let attacker = combat::Attacker {
            modifiers: combat::Modifiers {
                equipment: &worn,
                effects: &[],
            },
            ticks_since_reset: self
                .last_strike
                .map_or(u32::MAX, |at| cx.tick.saturating_sub(at) as u32),
            fall_distance: core.base.fall_distance,
            on_ground: core.base.on_ground,
            sprinting: false,
            in_water: false,
            climbable: false,
            known_speed_sq: 0.0,
        };
        let melee = combat::melee(&attacker, kind, cx.rng);
        self.last_strike = Some(cx.tick);
        let Some(melee) = melee else {
            return true;
        };
        let mut hit = EntityHit::new(melee.damage, &damage_type::PLAYER_ATTACK);
        hit.attacker = Some(Culprit::entity(&core.base));
        hit.position = Some(core.base.position);
        hit.weapon = core
            .equipment
            .main_hand()
            .map(tuff_item::item::ItemSnapshot::of);
        let (dx, dz) = (
            (bounds[0] + bounds[3]) / 2.0 - core.base.position[0],
            (bounds[2] + bounds[5]) / 2.0 - core.base.position[2],
        );
        let length = dx.hypot(dz).max(1e-4);
        if melee.knockback > 0.0 {
            hit.knockback = Some((f64::from(melee.knockback), dx / length, dz / length));
        }
        cx.effects.push(Effect::Hurt {
            target,
            hit: Box::new(hit),
        });
        true
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Mannequin, super::load::<Mannequin>);
}

#[cfg(test)]
mod tests {
    use super::super::Mob;
    use super::*;
    use tuff_core::world::entity::{EntityBase, RegionEntity, Tick};
    use tuff_vanilla_data::damage_type;

    fn mannequin(nbt: &mut Compound, y: f64) -> Mob<Mannequin> {
        let base = EntityBase::new(EntityType::Mannequin, 1, 1, [0.5, y, 0.5], [0.0; 3]);
        let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
        let mut mob = Mob::<Mannequin>::new(base, &mut rng);
        mob.species.load(&mut mob.core, nbt);
        mob
    }

    #[tuff_core::watchdog]
    #[test]
    fn data_indices_match_the_dumped_layout() {
        let layout = EntityType::Mannequin.data_layout();
        let entity = mannequin(&mut Compound::new(), 64.0);
        for (index, value) in entity.metadata() {
            assert_eq!(
                layout[index as usize],
                value.type_id() as u8,
                "index {index}"
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_falls_and_can_be_hurt_to_death() {
        super::super::tests::with_context(|_, cx| {
            let mut entity = mannequin(&mut Compound::new(), 70.0);
            for _ in 0..40 {
                entity.tick(cx);
            }
            assert!(entity.base().position[1] < 64.5, "fell to the ground");
            assert_eq!(entity.core.health, 17.0, "took fall damage");
            for _ in 0..20 {
                entity.tick(cx);
            }
            entity.hurt(5.0, &damage_type::GENERIC);
            entity.tick(cx);
            assert_eq!(entity.core.health, 12.0);
            entity.hurt(1000.0, &damage_type::GENERIC);
            let mut discarded = false;
            for _ in 0..40 {
                discarded |= matches!(entity.tick(cx), Tick::Discard);
            }
            assert!(discarded, "dies and is removed");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn standing_on_slime_slows_horizontal_motion() {
        use tuff_vanilla_data::Block;
        use tuff_world_model::BlockPos;
        let run = |floor: Block| {
            let block = move |pos: BlockPos| {
                Some(if pos.y < 64 {
                    floor.default_state()
                } else {
                    Block::Air.default_state()
                })
            };
            let mut travelled = 0.0;
            super::super::tests::with_world(&block, &[], |_, cx| {
                let mut entity = mannequin(&mut Compound::new(), 64.0);
                for _ in 0..3 {
                    entity.tick(cx);
                }
                entity.core.base.velocity = [0.3, 0.0, 0.0];
                let start = entity.core.base.position[0];
                entity.tick(cx);
                travelled = entity.core.base.position[0] - start;
            });
            travelled
        };
        let grass = run(Block::GrassBlock);
        let slime = run(Block::SlimeBlock);
        assert!(grass > 0.0);
        assert!(slime <= grass + 1.0e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn immovable_mannequins_still_take_damage() {
        super::super::tests::with_context(|_, cx| {
            let mut nbt = Compound::new().with("immovable", true);
            let mut entity = mannequin(&mut nbt, 64.0);
            entity.hurt(5.0, &damage_type::GENERIC);
            entity.tick(cx);
            assert_eq!(entity.core.health, 15.0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn hidden_layers_round_trip() {
        let mut nbt = Compound::new().with(
            "hidden_layers",
            Tag::List(vec![Tag::String("hat".into()), Tag::String("cape".into())]),
        );
        let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
        let mut mob = Mob::<Mannequin>::new(
            EntityBase::new(EntityType::Mannequin, 1, 1, [0.0; 3], [0.0; 3]),
            &mut rng,
        );
        mob.species.load(&mut mob.core, &mut nbt);
        assert_eq!(mob.species.shown_layers, ALL_LAYERS & !0x41);
        let mut out = Compound::new();
        mob.species.save(&mob.core, &mut out);
        assert_eq!(
            out.get("hidden_layers"),
            Some(&Tag::List(vec![
                Tag::String("cape".into()),
                Tag::String("hat".into())
            ]))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_saved_field_loads_sends_and_saves() {
        let snbt = r#"{profile:Dream,hidden_layers:[cape,hat],main_hand:left,pose:fall_flying,immovable:1b,description:"Test Doll",hide_description:1b}"#;
        let Tag::Compound(mut nbt) = Tag::from_snbt(snbt).unwrap() else {
            panic!("compound");
        };
        let mob = mannequin(&mut nbt, 64.0);
        assert!(nbt.is_empty(), "left over: {nbt:?}");
        let data = mob.species.metadata(&mob.core);
        let get = |i: u8| data.iter().find(|(j, _)| *j == i).unwrap().1.clone();
        assert_eq!(get(metadata::POSE), MetadataValue::Pose(1));
        assert_eq!(get(MAIN_ARM), MetadataValue::HumanoidArm(0));
        assert_eq!(get(SKIN_PARTS), MetadataValue::Byte(ALL_LAYERS & !0x41));
        assert_eq!(get(IMMOVABLE), MetadataValue::Boolean(true));
        assert_eq!(get(DESCRIPTION), MetadataValue::OptionalText(None));
        let MetadataValue::ResolvableProfile(profile) = get(PROFILE) else {
            panic!("profile");
        };
        assert_eq!(profile.name.as_deref(), Some("Dream"));
        assert_eq!(mob.core.size.height, 0.6);
        let mut out = Compound::new();
        mob.species.save(&mob.core, &mut out);
        for key in [
            "profile",
            "hidden_layers",
            "main_hand",
            "pose",
            "immovable",
            "description",
            "hide_description",
        ] {
            assert!(out.contains_key(key), "{key}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_skinned_named_and_equipped_body_loads_whole() {
        use tuff_vanilla_data::Item;
        use tuff_vanilla_data::item::EquipmentSlot;
        let snbt = r#"{Pos:[0.5d,65.0d,10.5d],profile:{texture:"minecraft:entity/player/wide/steve"},CustomName:{text:"Ashwin_Bot",color:"blue"},CustomNameVisible:1b,hide_description:1b,NoGravity:1b,Tags:["bedwars_bot"],equipment:{head:{id:"minecraft:leather_helmet",count:1,components:{"minecraft:dyed_color":3949738}},chest:{id:"minecraft:leather_chestplate",count:1},legs:{id:"minecraft:leather_leggings",count:1},feet:{id:"minecraft:leather_boots",count:1},mainhand:{id:"minecraft:wooden_sword",count:1}}}"#;
        let Tag::Compound(mut nbt) = Tag::from_snbt(snbt).unwrap() else {
            panic!("compound");
        };
        let base = EntityBase::load(EntityType::Mannequin, 1, &mut nbt).unwrap();
        let mut entity = super::super::load::<Mannequin>(base, &mut nbt).unwrap();
        assert!(!nbt.contains_key("equipment") && !nbt.contains_key("profile"));
        let mob = entity.downcast_mut::<Mob<Mannequin>>().unwrap();
        assert_eq!(
            mob.species.profile.texture.as_deref(),
            Some("minecraft:entity/player/wide/steve")
        );
        assert_eq!(mob.species.description(), None);
        assert!(mob.core.base.custom_name.is_some() && mob.core.base.custom_name_visible);
        assert!(mob.core.base.no_gravity);
        let worn = |slot| mob.core.equipment.get(slot).map(|s| s.item());
        assert_eq!(worn(EquipmentSlot::Head), Some(Item::LeatherHelmet));
        assert_eq!(worn(EquipmentSlot::Feet), Some(Item::LeatherBoots));
        assert_eq!(worn(EquipmentSlot::Mainhand), Some(Item::WoodenSword));
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_default_description_is_the_mannequin_label() {
        let mob = mannequin(&mut Compound::new(), 64.0);
        assert_eq!(
            mob.species.description(),
            Some(TextComponent::translate(DEFAULT_DESCRIPTION, []))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_spawn_egg_with_a_profile_names_the_mannequin_it_hatches() {
        let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
        let mut entity: Box<dyn tuff_core::world::entity::RegionEntity> =
            Box::new(Mob::<Mannequin>::new(
                EntityBase::new(EntityType::Mannequin, 1, 1, [0.0; 3], [0.0; 3]),
                &mut rng,
            ));
        let mut components = tuff_item::item::Components::default();
        components
            .other
            .insert("minecraft:profile", Tag::String("Dream".into()));
        crate::apply_egg_components(&mut entity, &components);
        let mob = entity.downcast_mut::<Mob<Mannequin>>().unwrap();
        assert_eq!(mob.species.profile.name.as_deref(), Some("Dream"));
        assert!(mob.core.changed.contains(&PROFILE));
        let mut out = Compound::new();
        mob.species.save(&mob.core, &mut out);
        assert_eq!(out.get("profile"), Some(&Tag::String("Dream".into())));
    }
}
