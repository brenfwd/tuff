//! The standard vanilla feature set: what turns an inert core into a
//! vanilla server. Order matters only where one feature's `Resolve` phase
//! reads another's registrations.

use tuff_core::feature::Feature;

pub fn all() -> Vec<Box<dyn Feature>> {
    vec![
        Box::new(tuff_feature_builtin_vanilla_worldgen::WorldgenFeature),
        Box::new(tuff_feature_builtin_vanilla_redstone::RedstoneFeature),
        Box::new(tuff_feature_builtin_vanilla_items::ItemsFeature),
        Box::new(tuff_feature_builtin_vanilla_maps::MapsFeature),
        Box::new(tuff_feature_builtin_vanilla_recipes::RecipesFeature),
        Box::new(tuff_feature_builtin_vanilla_loot::LootFeature),
        Box::new(tuff_feature_builtin_vanilla_blocks::BlocksFeature),
        Box::new(tuff_feature_builtin_vanilla_combat::CombatFeature),
        Box::new(tuff_feature_builtin_vanilla_players::PlayersFeature),
        Box::new(tuff_feature_builtin_vanilla_entities::EntitiesFeature),
        Box::new(tuff_feature_builtin_vanilla_mobs::MobsFeature),
        Box::new(tuff_feature_builtin_vanilla_survival::SurvivalFeature),
        Box::new(tuff_feature_builtin_vanilla_dimensions::DimensionsFeature),
        Box::new(tuff_feature_builtin_core_commands::CoreCommandsFeature),
        Box::new(tuff_feature_builtin_vanilla_commands::VanillaCommandsFeature),
        Box::new(tuff_feature_builtin_vanilla_anticheat::AnticheatFeature),
    ]
}
