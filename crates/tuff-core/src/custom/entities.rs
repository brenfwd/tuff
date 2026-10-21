//! Custom entities: a vanilla stand-in entity tagged with the custom id,
//! carrying display and interaction entities as passengers. The passengers
//! follow the stand-in through the vehicle link, so position sync and the
//! save format are the vanilla ones.

use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::EntityType;

use super::{CustomContent, CustomEntityDef, entity_tag};

pub const MAX_PARTS: usize = 8;

/// One composed entity: its type and the saved data it starts with.
#[derive(Debug, Clone)]
pub struct EntityPart {
    pub kind: EntityType,
    pub data: Compound,
}

impl EntityPart {
    pub fn new(kind: EntityType, data: Compound) -> Self {
        Self { kind, data }
    }

    /// An invisible interaction entity of `width` by `height`: the hitbox
    /// clients target for attacks and use.
    pub fn hitbox(width: f32, height: f32) -> Self {
        Self::new(
            EntityType::Interaction,
            Compound::new()
                .with("width", width)
                .with("height", height)
                .with("response", 1i8),
        )
    }
}

pub fn part_kind_allowed(kind: EntityType) -> bool {
    matches!(
        kind,
        EntityType::ItemDisplay
            | EntityType::BlockDisplay
            | EntityType::TextDisplay
            | EntityType::Interaction
            | EntityType::Marker
    )
}

impl CustomEntityDef {
    /// The saved entity for an instance at `position`: the stand-in with the
    /// identity tag and every part nested in `Passengers`.
    pub fn spawn_nbt(&self, position: [f64; 3], yaw: f32) -> Compound {
        let pos = |p: [f64; 3]| Tag::List(p.map(Tag::Double).to_vec());
        let passengers = self
            .parts
            .iter()
            .map(|part| {
                let mut nbt = part.data.clone();
                nbt.insert("id", part.kind.name());
                nbt.insert("Pos", pos(position));
                Tag::Compound(nbt)
            })
            .collect::<Vec<_>>();
        let mut nbt = Compound::new()
            .with("id", self.stand_in.name())
            .with("Pos", pos(position))
            .with(
                "Rotation",
                Tag::List(vec![Tag::Float(yaw), Tag::Float(0.0)]),
            )
            .with("Tags", Tag::List(vec![Tag::from(entity_tag(&self.id))]));
        if self.invisible {
            nbt.insert("Invisible", true);
        }
        if !passengers.is_empty() {
            nbt.insert("Passengers", Tag::List(passengers));
        }
        nbt
    }
}

impl CustomContent {
    /// The saved entity for a new instance of custom entity `id`, ready for
    /// the region's saved-entity spawn path.
    pub fn spawn_entity_nbt(&self, id: &str, position: [f64; 3], yaw: f32) -> Option<Compound> {
        Some(self.entity(id)?.spawn_nbt(position, yaw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom::{Definitions, entity_id};

    fn golem() -> CustomEntityDef {
        CustomEntityDef::new("ex:golem", EntityType::IronGolem)
            .with_part(EntityPart::new(
                EntityType::ItemDisplay,
                Compound::new().with("item_display", "head"),
            ))
            .with_part(EntityPart::hitbox(1.5, 3.0))
    }

    #[tuff_core::watchdog]
    #[test]
    fn invisible_stand_ins_save_the_flag() {
        let nbt = golem().invisible().spawn_nbt([0.0; 3], 0.0);
        assert_eq!(nbt.get_bool("Invisible"), Some(true));
        assert!(
            golem()
                .spawn_nbt([0.0; 3], 0.0)
                .get_bool("Invisible")
                .is_none()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn spawn_nbt_tags_the_stand_in_and_nests_the_parts() {
        let mut defs = Definitions::default();
        defs.register_entity("ex", golem()).unwrap();
        let content = defs.resolve().unwrap();
        let nbt = content
            .spawn_entity_nbt("ex:golem", [1.0, 64.0, 2.0], 90.0)
            .unwrap();
        assert_eq!(nbt.get_str("id"), Some("minecraft:iron_golem"));
        let tags: Vec<String> = nbt
            .get("Tags")
            .and_then(Tag::as_list)
            .unwrap()
            .iter()
            .filter_map(|t| t.as_str().map(str::to_owned))
            .collect();
        assert_eq!(entity_id(&tags), Some("ex:golem"));
        let parts = nbt.get("Passengers").and_then(Tag::as_list).unwrap();
        let kinds: Vec<_> = parts
            .iter()
            .map(|p| p.as_compound().unwrap().get_str("id").unwrap())
            .collect();
        assert_eq!(kinds, ["minecraft:item_display", "minecraft:interaction"]);
        assert!(content.spawn_entity_nbt("ex:none", [0.0; 3], 0.0).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn parts_must_be_display_like_entities() {
        let mut defs = Definitions::default();
        let bad = CustomEntityDef::new("ex:bad", EntityType::Cow)
            .with_part(EntityPart::new(EntityType::Zombie, Compound::new()));
        assert!(defs.register_entity("ex", bad).is_err());
    }
}
