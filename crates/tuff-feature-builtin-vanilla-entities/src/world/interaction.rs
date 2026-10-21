//! Interaction entities: invisible, data-sized boxes that remember the
//! last player to attack and to use them, for datapacks to read.

use tuff_vanilla_data::registry::EntityType;

use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{
    AttackOutcome, EntityAttack, EntityBase, EntityInteraction, InteractOutcome, RegionEntity,
    Tick, TickContext, number,
};
use tuff_core::world::messages::HandEffect;
use tuff_nbt::{Compound, Tag};

const DATA_WIDTH: u8 = 8;
const DATA_HEIGHT: u8 = 9;
const DATA_RESPONSE: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerAction {
    pub player: u128,
    pub timestamp: i64,
}

impl PlayerAction {
    fn load(tag: &Tag) -> Option<Self> {
        let nbt = tag.as_compound()?;
        let player = match nbt.get("player")? {
            Tag::IntArray(v) if v.len() == 4 => v
                .iter()
                .fold(0u128, |acc, part| (acc << 32) | u128::from(*part as u32)),
            _ => return None,
        };
        Some(Self {
            player,
            timestamp: number(nbt.get("timestamp")?)? as i64,
        })
    }

    fn save(&self) -> Tag {
        let parts = (0..4)
            .map(|i| (self.player >> (96 - 32 * i)) as u32 as i32)
            .collect();
        Tag::Compound(
            Compound::new()
                .with("player", Tag::IntArray(parts))
                .with("timestamp", self.timestamp),
        )
    }
}

#[derive(Debug)]
pub struct Interaction {
    base: EntityBase,
    width: f32,
    height: f32,
    response: bool,
    attack: Option<PlayerAction>,
    interaction: Option<PlayerAction>,
}

impl Interaction {
    pub fn attack(&self) -> Option<PlayerAction> {
        self.attack
    }

    pub fn interaction(&self) -> Option<PlayerAction> {
        self.interaction
    }

    pub fn size(&self) -> (f32, f32) {
        (self.width, self.height)
    }
}

pub fn load(mut base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let float = |nbt: &mut Compound, key: &str, default: f32| {
        tuff_core::world::entity::take_number(nbt, key).map_or(default, |v| v as f32)
    };
    let width = float(nbt, "width", 1.0);
    let height = float(nbt, "height", 1.0);
    let attack = tuff_core::world::entity::take(nbt, "attack", PlayerAction::load);
    let interaction = tuff_core::world::entity::take(nbt, "interaction", PlayerAction::load);
    let response = tuff_core::world::entity::take_bool(nbt, "response").unwrap_or(false);
    base.no_gravity = true;
    Some(Box::new(Interaction {
        base,
        width,
        height,
        response,
        attack,
        interaction,
    }))
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Interaction, load);
}

impl RegionEntity for Interaction {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert("width", self.width);
        out.insert("height", self.height);
        out.insert("response", self.response);
        if let Some(attack) = &self.attack {
            out.insert("attack", attack.save());
        }
        if let Some(interaction) = &self.interaction {
            out.insert("interaction", interaction.save());
        }
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![
            (DATA_WIDTH, MetadataValue::Float(self.width)),
            (DATA_HEIGHT, MetadataValue::Float(self.height)),
            (DATA_RESPONSE, MetadataValue::Boolean(self.response)),
        ]
    }

    fn bounds(&self) -> [f64; 6] {
        let [x, y, z] = self.base.position;
        let half = f64::from(self.width) / 2.0;
        [
            x - half,
            y,
            z - half,
            x + half,
            y + f64::from(self.height),
            z + half,
        ]
    }

    fn pickable(&self) -> bool {
        true
    }

    fn attacked(&mut self, attack: &EntityAttack, cx: &mut TickContext) -> Option<AttackOutcome> {
        self.attack = Some(PlayerAction {
            player: attack.attacker_uuid,
            timestamp: cx.tick as i64,
        });
        Some(AttackOutcome {
            hurt: false,
            health_lost: 0.0,
        })
    }

    fn interact(
        &mut self,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        self.interaction = Some(PlayerAction {
            player: interaction.player_uuid,
            timestamp: cx.tick as i64,
        });
        Some(HandEffect::None)
    }

    fn tick(&mut self, _cx: &mut TickContext) -> Tick {
        Tick::Alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn records_and_size_round_trip() {
        let mut registry = tuff_core::world::entity::Registry::default();
        register(&mut registry);
        let nbt = Compound::new()
            .with("id", "minecraft:interaction")
            .with(
                "Pos",
                Tag::List([1.0f64, 2.0, 3.0].map(Tag::Double).to_vec()),
            )
            .with("width", 2.0f32)
            .with("height", 0.5f32)
            .with("response", true)
            .with(
                "attack",
                Compound::new()
                    .with("player", Tag::IntArray(vec![1, 2, 3, 4]))
                    .with("timestamp", 77i64),
            );
        let loaded = registry.load(&nbt, 1).expect("loads");
        let entity = loaded.downcast_ref::<Interaction>().expect("interaction");
        assert_eq!(entity.size(), (2.0, 0.5));
        assert_eq!(
            entity.attack().map(|a| (a.player, a.timestamp)),
            Some((0x0000_0001_0000_0002_0000_0003_0000_0004, 77))
        );
        assert!(entity.interaction().is_none());
        assert_eq!(entity.bounds(), [0.0, 2.0, 2.0, 2.0, 2.5, 4.0]);
        let saved = tuff_core::world::entity::save(entity);
        assert_eq!(saved.get_bool("response"), Some(true));
        assert_eq!(
            saved.get_compound("attack").unwrap().get("player"),
            Some(&Tag::IntArray(vec![1, 2, 3, 4]))
        );
        let defaults = Compound::new()
            .with("id", "minecraft:interaction")
            .with("Pos", Tag::List([0.0f64; 3].map(Tag::Double).to_vec()));
        let fresh = registry.load(&defaults, 2).unwrap();
        assert_eq!(
            fresh.downcast_ref::<Interaction>().unwrap().size(),
            (1.0, 1.0)
        );
    }
}
