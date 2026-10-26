//! The vanilla loot tables: block drops, container loot and entity drops.

#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "test code is exempt from the door gate"
    )
)]

pub use tuff_core::world::exploration;
pub use tuff_item::item::enchanting;
mod loot;

pub use loot::*;

/// Vanilla block loot tables, answering core's drop questions.
#[derive(Default)]
pub struct VanillaLoot {
    tables: std::sync::Arc<tuff_datapack::loot::LootTables>,
}

impl tuff_core::world::drops::BlockLoot for VanillaLoot {
    fn with_tables(
        &self,
        tables: std::sync::Arc<tuff_datapack::loot::LootTables>,
    ) -> Option<std::sync::Arc<dyn tuff_core::world::drops::BlockLoot>> {
        Some(std::sync::Arc::new(VanillaLoot { tables }))
    }

    fn lock_admits(
        &self,
        lock: &tuff_nbt::Compound,
        held: Option<tuff_item::item::AnyItem<'_>>,
    ) -> bool {
        match held {
            None => loot::lock_admits::<tuff_item::item::Counted>(lock, None),
            Some(held) => tuff_item::with_item!(held, |held| loot::lock_admits(lock, Some(held))),
        }
    }

    fn container_loot(
        &self,
        name: &str,
        slots: usize,
        rng: &mut rand::rngs::StdRng,
    ) -> Option<Vec<Option<tuff_item::item::ItemStack>>> {
        loot::container_loot_in(&self.tables, name, slots, rng)
    }

    fn explore(
        &self,
        shared: &std::sync::Arc<tuff_core::world::Shared>,
        dimension: tuff_core::world::dimension::DimensionId,
        origin: tuff_world_model::BlockPos,
        stack: &tuff_item::item::ItemStack,
    ) -> Option<tuff_core::world::exploration::Answer> {
        exploration::finish(shared, dimension, origin, stack)
    }

    fn modify_item(
        &self,
        functions: &[tuff_vanilla_data::loot_table::LootFunction],
        stack: tuff_item::item::ItemStack,
        rng: &mut rand::rngs::StdRng,
    ) -> tuff_item::item::ItemStack {
        loot::modify_item(functions, stack, rng)
    }

    fn roll(&self, name: &str, rng: &mut rand::rngs::StdRng) -> Vec<tuff_item::item::ItemStack> {
        loot::roll_in(&self.tables, name, rng)
    }

    fn entity_drops(
        &self,
        name: &str,
        facts: &EntityFacts,
        rng: &mut rand::rngs::StdRng,
    ) -> Vec<tuff_item::item::ItemStack> {
        loot::entity_drops(name, facts, rng)
    }

    fn fishing(
        &self,
        luck: f32,
        open_water: bool,
        rod: tuff_item::item::AnyItem<'_>,
        rng: &mut rand::rngs::StdRng,
    ) -> Vec<tuff_item::item::ItemStack> {
        loot::fishing_drops(&self.tables, luck, open_water, rod, rng)
    }

    fn can_harvest(
        &self,
        state: tuff_vanilla_data::BlockState,
        tool: Option<tuff_item::item::AnyItem<'_>>,
    ) -> bool {
        loot::can_harvest(state, tool)
    }

    fn drops(
        &self,
        block: tuff_core::world::block_ledger::BrokenBlock,
        tool: Option<tuff_item::item::AnyItem<'_>>,
        rng: &mut rand::rngs::StdRng,
    ) -> Vec<tuff_item::item::ItemStack> {
        loot::block_drops_in(&self.tables, block.state(), tool, block.block_entity(), rng)
    }

    fn drops_at(
        &self,
        block: tuff_core::world::block_ledger::BrokenBlock,
        tool: Option<tuff_item::item::AnyItem<'_>>,
        at: &tuff_core::world::predicate::LocationFacts,
        world: Option<&dyn tuff_core::world::predicate::PredicateWorld>,
        rng: &mut rand::rngs::StdRng,
    ) -> Vec<tuff_item::item::ItemStack> {
        let (state, block_entity) = (block.state(), block.block_entity());
        loot::block_drops_located(&self.tables, state, tool, block_entity, at, world, rng)
    }

    fn destroyed(
        &self,
        block: tuff_core::world::block_ledger::BrokenBlock,
        rng: &mut rand::rngs::StdRng,
    ) -> Vec<tuff_item::item::ItemStack> {
        loot::destroyed_drops_in(&self.tables, block.state(), block.block_entity(), rng)
    }

    fn table(
        &self,
        name: &str,
        state: tuff_vanilla_data::BlockState,
        tool: Option<tuff_vanilla_data::Item>,
        rng: &mut rand::rngs::StdRng,
    ) -> Vec<tuff_item::item::ItemStack> {
        loot::table_drops_in(&self.tables, name, state, tool, rng)
    }

    fn exploded(
        &self,
        block: tuff_core::world::block_ledger::BrokenBlock,
        decay: Option<f32>,
        rng: &mut rand::rngs::StdRng,
    ) -> Vec<tuff_item::item::ItemStack> {
        let (state, block_entity) = (block.state(), block.block_entity());
        loot::exploded_drops_in(&self.tables, state, decay, block_entity, rng)
    }
}

/// Registers vanilla loot into core.
pub struct LootFeature;

impl tuff_core::feature::Feature for LootFeature {
    fn id(&self) -> &'static str {
        "vanilla-loot"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let id = cx.feature_id();
            cx.registries()
                .block_loot
                .fill(id, std::sync::Arc::new(VanillaLoot::default()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use tuff_core::world::drops::{BlockLoot, NoBlockLoot};
    use tuff_item::item::{ItemStack, MintReason};
    use tuff_vanilla_data::Item;
    use tuff_vanilla_data::registry::EntityType;

    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_loot_seam_rolls_tables_entities_and_fishing() {
        let loot = VanillaLoot::default();
        let mut rng = StdRng::seed_from_u64(3);
        assert!((0..8).any(|_| {
            !loot
                .roll("minecraft:chests/simple_dungeon", &mut rng)
                .is_empty()
        }));
        let facts = EntityFacts {
            kind: Some(EntityType::Cow),
            ..EntityFacts::default()
        };
        let drops = loot.entity_drops("minecraft:entities/cow", &facts, &mut rng);
        assert!(drops.iter().any(|s| s.item() == Item::Beef));
        let rod = ItemStack::mint(Item::FishingRod, 1, MintReason::Test);
        assert_eq!(loot.fishing(0.0, true, rod.any(), &mut rng).len(), 1);

        let none = NoBlockLoot;
        assert!(
            none.roll("minecraft:chests/simple_dungeon", &mut rng)
                .is_empty()
        );
        assert!(none.fishing(0.0, true, rod.any(), &mut rng).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_entity_table_sees_its_origin_for_location_checks() {
        let json = serde_json::json!({"pools": [{"rolls": 1, "entries": [{
            "type": "minecraft:item", "name": "minecraft:diamond",
            "condition": {"type": "minecraft:location_check",
                "predicate": {"dimension": "minecraft:the_nether"}}
        }]}]});
        let table = tuff_datapack::loot::parse(
            json.to_string().as_bytes(),
            &tuff_datapack::tags::TagSet::vanilla(),
            &tuff_datapack::loot::Predicates::vanilla(),
        )
        .unwrap();
        let tables = std::sync::Arc::new(tuff_datapack::loot::LootTables::new(
            [("test:reward".to_owned(), Some(table))]
                .into_iter()
                .collect(),
        ));
        let loot = VanillaLoot::default();
        let mut rng = StdRng::seed_from_u64(1);
        let facts_in = |dimension: &str| EntityFacts {
            kind: Some(EntityType::Player),
            tables: Some(tables.clone()),
            origin: Some(tuff_core::world::predicate::LocationFacts {
                dimension: dimension.to_owned(),
                ..Default::default()
            }),
            ..EntityFacts::default()
        };
        let nether = loot.entity_drops("test:reward", &facts_in("minecraft:the_nether"), &mut rng);
        assert_eq!(nether.len(), 1);
        let overworld =
            loot.entity_drops("test:reward", &facts_in("minecraft:overworld"), &mut rng);
        assert!(overworld.is_empty());
    }
}
