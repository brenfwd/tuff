use super::fixtures::*;
use crate::biome_zoom::BiomeLookup;
use crate::density::{Context, DensityVolume};
use crate::surface::{CompiledCondition, RuleContext};
use tuff_world_model::chunk::biome_id;

struct One(u16);

impl BiomeLookup for One {
    fn biome(&mut self, _: i32, _: i32, _: i32) -> u16 {
        self.0
    }
}

fn holds(biome: &str, y: i32) -> bool {
    let generator = generator("overworld");
    let volume = DensityVolume::blocks([1, 1, 1], [0, 0, 0]);
    let mut rules = RuleContext::new(
        &generator.surface,
        &generator.surface.template(generator.span),
        volume,
    );
    rules.update_xz(0, 0, 0, 0);
    rules.update_y(0, 0, 0, y);
    let mut ctx = Context::builder().build();
    let mut biomes = One(biome_id(biome).unwrap());
    rules.test(&mut ctx, &mut biomes, &CompiledCondition::Temperature)
}

#[tuff_core::watchdog]
#[test]
fn the_temperature_condition_holds_where_the_biome_is_cold_enough_to_snow() {
    assert!(holds("minecraft:snowy_plains", 70));
    assert!(!holds("minecraft:desert", 70));
    assert!(holds("minecraft:plains", 2000));
}
