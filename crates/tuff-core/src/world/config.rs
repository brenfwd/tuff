use super::*;

/// Why a world with no dimension at all does not start.
pub const NO_DIMENSIONS: &str = "the world has no dimensions: every vanilla dimension is switched off \
     (tuff.world.dimensions.<overworld|nether|end>.enabled) and no plugin adds one; enable one, or \
     install a plugin that adds a dimension and grant it with `tuff plugin grant <id> dimension <key>`";

/// The configuration a server starts with, writing default properties if
/// there are none yet. A bad icon is reported and skipped.
pub fn load_config(
    store: &dyn ConfigStore,
    load_icon: bool,
) -> io::Result<(
    ServerProperties,
    tuff_storage::admin::UserLists,
    Option<Favicon>,
)> {
    let properties = match store.read_properties()? {
        Some(properties) => properties,
        None => {
            let properties = ServerProperties::default();
            store.write_properties(&properties)?;
            properties
        }
    };
    let lists = store.read_lists()?;
    let favicon = if load_icon {
        match store.read_icon() {
            Ok(Some(png)) => Favicon::from_png(&png)
                .inspect_err(|err| log::warn!("ignoring the server icon: {err:#}"))
                .ok(),
            Ok(None) => None,
            Err(err) => {
                log::warn!("cannot read the server icon: {err}");
                None
            }
        }
    } else {
        None
    };
    Ok((properties, lists, favicon))
}

/// The parts of [`Shared`] that come from the frozen feature registries,
/// each falling back to an inert default when no feature filled it.
pub struct Resolved {
    pub commands: Arc<commands::Registry>,
    pub entity_types: Arc<entity::Registry>,
    pub advancements: Arc<dyn advancement::Advancements>,
    pub redstone_behaviors: Arc<dyn redstone::BlockBehaviors>,
    pub station_rules: Arc<dyn stations::StationRules>,
    pub growth_behaviors: Arc<dyn growth::GrowthBehaviors>,
    pub block_loot: Arc<dyn drops::BlockLoot>,
    pub tnt: Option<Arc<dyn explosion::TntPrimer>>,
    pub explosion_rules: Arc<dyn explosion::ExplosionRules>,
    pub block_rules: Arc<dyn block_rules::BlockRules>,
    pub dispensers: Option<Arc<dyn region_api::Dispensers>>,
    pub block_entity_carrier: Option<Arc<dyn redstone::BlockEntityCarrier>>,
    pub block_entity_tickers: Arc<block_entity::BlockEntityTickers>,
    pub item_ports: Arc<tuff_item::registry::BlockTable<dyn item_port::ItemPort>>,
    pub region_hooks: Arc<[Arc<dyn region_api::RegionHooks>]>,
    pub session_hooks: Arc<[Arc<dyn session::hooks::SessionHooks>]>,
    pub player_saved: Arc<[Arc<Box<dyn session::hooks::PlayerSaved>>]>,
    pub login_gates: Arc<[Arc<dyn login_gate::LoginGates>]>,
    pub player_damage: Option<Arc<dyn session::hooks::PlayerDamage>>,
    pub fake_motion: Option<Arc<dyn fake::motion::FakeMotion>>,
    /// Packet interceptors, sorted by order.
    pub interceptors: Arc<[session::hooks::Interceptor]>,
    pub save_normalisers: crate::storage::normalise::SaveNormalisers,
    pub recipe_set: Arc<crafting::RecipeSet>,
    pub data_recipes: Option<Arc<dyn crafting::DataRecipes>>,
    pub fluid_behaviors: Arc<dyn fluid_api::FluidBehaviors>,
    pub anti_xray: Arc<dyn anti_xray::AntiXrayPolicy>,
    /// Hook owners, with interceptors in the sorted order above.
    pub hook_owners: Arc<crate::feature::HookOwners>,
    pub custom: Arc<crate::custom::CustomContent>,
}

impl Resolved {
    pub fn from_registries(registries: &crate::registry::Registries) -> Self {
        Self {
            commands: Arc::new(registries.commands.clone()),
            entity_types: Arc::new(registries.entity_types.clone()),
            advancements: registries
                .advancements
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(advancement::NoAdvancements)),
            redstone_behaviors: registries
                .block_behaviors
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(redstone::NoBlockBehaviors)),
            station_rules: registries
                .station_rules
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(stations::NoStationRules)),
            growth_behaviors: registries
                .growth_behaviors
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(growth::NoGrowthBehaviors)),
            block_loot: registries
                .block_loot
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(drops::NoBlockLoot)),
            tnt: registries.tnt.get().cloned(),
            block_rules: registries
                .block_rules
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(block_rules::NoBlockRules)),
            explosion_rules: registries
                .explosion_rules
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(explosion::NoExplosionRules)),
            dispensers: registries.dispensers.get().cloned(),
            block_entity_carrier: registries.block_entity_carrier.get().cloned(),
            block_entity_tickers: Arc::new(registries.block_entity_tickers.clone()),
            item_ports: Arc::new(registries.item_ports.clone()),
            region_hooks: registries.region_hooks.iter().cloned().collect(),
            session_hooks: registries.session_hooks.iter().cloned().collect(),
            player_saved: registries
                .player_saved
                .iter()
                .map(|(_, s)| s.clone())
                .collect(),
            login_gates: registries.login_gates.iter().cloned().collect(),
            player_damage: registries.player_damage.get().cloned(),
            fake_motion: registries.fake_motion.get().cloned(),
            save_normalisers: crate::storage::normalise::SaveNormalisers::new(
                registries.save_normalisers.clone(),
            ),
            interceptors: sorted_interceptors(registries)
                .into_iter()
                .map(|(interceptor, _)| interceptor)
                .collect(),
            hook_owners: Arc::new(crate::feature::HookOwners {
                packet_interceptors: sorted_interceptors(registries)
                    .into_iter()
                    .map(|(_, owner)| owner)
                    .collect(),
                ..registries.hook_owners.clone()
            }),
            recipe_set: Arc::new(
                crafting::RecipeSet::from_registry(&registries.recipe_entries)
                    .with_custom(registries.custom.content()),
            ),
            data_recipes: registries.data_recipes.get().cloned(),
            fluid_behaviors: registries
                .fluid_behaviors
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(fluid_api::NoFluidBehaviors)),
            anti_xray: registries
                .anti_xray
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(anti_xray::ConfigPolicy)),
            custom: registries.custom.content(),
        }
    }

    /// These with the content `registries`' content providers add (see
    /// `crate::custom::provided`): custom content, the recipes and
    /// advancements they define and command arguments for their items
    /// and blocks.
    pub fn with_provided(
        mut self,
        registries: &crate::registry::Registries,
        ctx: &custom_dimension::ProviderContext,
    ) -> Self {
        let provided = crate::custom::provided::collect(&registries.content_providers, ctx);
        if provided.is_empty() {
            return self;
        }
        let merged = crate::custom::provided::merge(
            &registries.custom,
            &self.recipe_set,
            &self.advancements,
            registries.data_recipes.get(),
            provided,
        );
        let commands = Arc::make_mut(&mut self.commands);
        for content in &merged.accepted {
            for def in &content.items {
                if let Some(def) = merged.custom.item(&def.id) {
                    commands.add_custom_item(
                        def.id.clone(),
                        crate::command::item::ItemInput::from_template(&def.template()),
                    );
                }
            }
        }
        for id in &merged.recipe_ids {
            commands.add_resource("minecraft:recipe", id);
        }
        for id in &merged.advancement_ids {
            commands.add_resource("minecraft:advancement", id);
        }
        self.custom = merged.custom;
        self.recipe_set = merged.recipes;
        self.advancements = merged.advancements;
        self
    }
}

pub(super) fn sorted_interceptors(
    registries: &crate::registry::Registries,
) -> Vec<(session::hooks::Interceptor, &'static str)> {
    let owners = &registries.hook_owners.packet_interceptors;
    let mut chain: Vec<_> = registries
        .packet_interceptors
        .iter()
        .enumerate()
        .map(|(i, interceptor)| {
            (
                interceptor.clone(),
                crate::feature::HookOwners::owner(owners, i),
            )
        })
        .collect();
    chain.sort_by_key(|(i, _)| i.order);
    chain
}
