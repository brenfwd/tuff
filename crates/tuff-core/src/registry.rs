//! [`Registries`]: the bag of registries a [`crate::feature::Feature`]
//! registers into. The generic primitives live in `tuff_item::registry`.

use std::sync::Arc;

use tuff_item::registry::{AnyRegistries, BlockTable, HookSlot, Registry};

/// The registries a `Feature::load` call registers into. Core-owned
/// concrete fields are for registries core's own mechanism reads
/// directly and so must be able to name the value type of; everything
/// else -- including most vanilla content -- goes through `ext`. A
/// field belongs here only once the trait it holds is defined in core.
pub struct Registries {
    /// The per-block table `region/redstone.rs`'s update/signal cascade
    /// dispatches through. Filled by `vanilla-redstone`; empty (and so
    /// every block inert) with no such feature. See
    /// `crate::world::redstone`.
    pub block_behaviors: HookSlot<dyn crate::world::redstone::BlockBehaviors>,

    /// Random ticks, scheduled ticks, placement and neighbour reactions
    /// and bone meal, dispatched by `region/growth.rs`. Filled by
    /// `vanilla-blocks`; with no feature every block is inert. See
    /// `crate::world::growth`.
    pub growth_behaviors: HookSlot<dyn crate::world::growth::GrowthBehaviors>,

    /// Placement, survival, shape and block tick rules. Filled by
    /// `vanilla-blocks`; with no feature blocks place in their default
    /// state and never react. See `crate::world::block_rules`.
    pub block_rules: HookSlot<dyn crate::world::block_rules::BlockRules>,

    /// Anvil, grindstone and loom results. Filled by `vanilla-items`; with no
    /// feature the stations offer nothing.
    pub station_rules: HookSlot<dyn crate::world::stations::StationRules>,

    /// What blocks drop. Filled by `vanilla-recipes`, which owns loot
    /// tables; with no feature nothing drops.
    pub block_loot: HookSlot<dyn crate::world::drops::BlockLoot>,

    /// Makes primed TNT when redstone, fire or an explosion lights it.
    /// Filled by `vanilla-combat`.
    pub tnt: HookSlot<dyn crate::world::explosion::TntPrimer>,

    /// What explosion rays meet and spare, the fire they light and what
    /// wind bursts do to blocks. Filled by `vanilla-combat`; with no
    /// feature rays meet plain block resistance.
    pub explosion_rules: HookSlot<dyn crate::world::explosion::ExplosionRules>,

    /// Fires a dispenser or dropper when redstone activates it. Filled by
    /// `vanilla-redstone`; with no feature dispensers and droppers do nothing.
    pub dispensers: HookSlot<dyn crate::world::region_api::Dispensers>,

    /// Lets pistons move block entity blocks with their data. Empty keeps
    /// the game's rule: block entities never move.
    pub block_entity_carrier: HookSlot<dyn crate::world::redstone::BlockEntityCarrier>,

    /// What hoppers, droppers and carts offer to and pull from blocks that
    /// are not modeled containers. See `crate::world::item_port`.
    pub item_ports: BlockTable<dyn crate::world::item_port::ItemPort>,

    /// Region behaviour (weather, spawning, block entities features keep
    /// raw, ...), called in registration order. See
    /// `crate::world::region_api`.
    pub region_hooks: Vec<Arc<dyn crate::world::region_api::RegionHooks>>,

    /// Player session behaviour, called in registration order. See
    /// `crate::world::session::hooks`.
    pub session_hooks: Vec<Arc<dyn crate::world::session::hooks::SessionHooks>>,

    /// Serverbound packet interceptors, run before everything else in the
    /// session in ascending order. See `crate::world::session::hooks`.
    pub packet_interceptors: Vec<crate::world::session::hooks::Interceptor>,

    /// Login throttles and other gates, asked in registration order. See
    /// `crate::world::login_gate`.
    pub login_gates: Vec<Arc<dyn crate::world::login_gate::LoginGates>>,

    /// Hooks that rewrite chunk and entity data about to be saved; see
    /// `crate::storage::normalise`. Empty means saves are untouched.
    pub save_normalisers: Vec<crate::storage::normalise::SaveNormaliserEntry>,

    /// How fluids flow. Filled by `vanilla-blocks`; with no feature fluids
    /// never move. See `crate::world::fluid_api`.
    pub fluid_behaviors: HookSlot<dyn crate::world::fluid_api::FluidBehaviors>,

    /// Every recipe by name: crafting, cooking, stonecutting, smithing,
    /// brewing and the recipe book. Empty with no feature: nothing crafts,
    /// cooks or brews and the book stays empty.
    pub recipe_entries: crate::world::crafting::Registry,

    /// Turns the recipes data packs define into recipes. Filled by
    /// `vanilla-recipes`; with no feature pack recipes are ignored.
    pub data_recipes: HookSlot<dyn crate::world::crafting::DataRecipes>,

    /// Services features run beside each world (see
    /// `crate::world::services`).
    pub services: Vec<Arc<dyn crate::world::services::ServiceFactory>>,

    /// The world generators; one feature owns them. With none every
    /// dimension is a void.
    pub generators: HookSlot<dyn crate::world::generator::WorldGenerators>,

    /// Dimensions features add beside the level's own, asked once per
    /// world start. See `crate::world::custom_dimension`.
    pub dimension_providers: Vec<Arc<dyn crate::world::custom_dimension::DimensionProvider>>,

    /// Custom content (items, blocks, entities, assets, recipes,
    /// advancements) providers add beside `custom`, asked once per world
    /// start. See `crate::custom::provided`.
    pub content_providers: Vec<Arc<dyn crate::custom::provided::ContentProvider>>,

    /// How players take damage; one feature owns it.
    pub player_damage: HookSlot<dyn crate::world::session::hooks::PlayerDamage>,

    /// Client physics for fake players; one feature owns it.
    pub fake_motion: HookSlot<dyn crate::world::fake::motion::FakeMotion>,

    /// Builds each dimension's anti-xray plan; empty reads the
    /// `tuff.antiXray.*` settings. See `crate::world::anti_xray`.
    pub anti_xray: HookSlot<dyn crate::world::anti_xray::AntiXrayPolicy>,

    /// Every command the parser, the Commands packet and the console
    /// know. Empty with no feature: every command is unknown.
    pub commands: crate::world::commands::Registry,

    /// Region entity loaders (and natural spawn rules) by saved type id.
    /// Empty with no feature: every saved entity stays raw NBT and
    /// `/summon` knows nothing.
    pub entity_types: crate::world::entity::Registry,

    /// The advancement tree sessions track progress against. Core builds
    /// it from the packs, definitions and triggers features register when
    /// no feature fills this slot first.
    pub advancements: HookSlot<dyn crate::world::advancement::Advancements>,

    /// Advancement triggers features add beside the vanilla ones, by
    /// trigger id; whoever fills `advancements` builds them into its
    /// tree in `Resolve`. Features fire them with `Event::Custom`.
    pub advancement_triggers: Registry<crate::world::advancement::trigger::Handler>,

    /// Whole advancement trees compiled in as data (the vanilla one), by
    /// pack name. The default tree merges them with the definitions.
    pub advancement_packs: Registry<&'static [tuff_vanilla_data::advancement::Advancement]>,

    /// Advancements features add beside the data pack's, by name; the
    /// default tree merges them in after `Resolve`.
    pub advancement_definitions: Registry<crate::world::advancement::Definition>,

    /// Per-player records features keep, by feature id. See
    /// `crate::world::session::hooks::PlayerSaved`.
    pub player_saved: Registry<Box<dyn crate::world::session::hooks::PlayerSaved>>,

    /// Runtime config keys features add beside the `tuff.*` ones; names
    /// should be namespaced by the feature (`myfeature.some.key`).
    pub config_keys: Vec<crate::config::Key>,

    /// HTTP routes features add beside core's own, as (path prefix,
    /// handler) pairs applied to the main listener at startup. See
    /// `tuff_http`.
    pub http_routes: Vec<(String, tuff_http::Handler)>,
    /// Resource packs hosted over HTTP and offered to every client at
    /// login (unless `offer_on_join` is off). Runtime changes go through
    /// `Shared::resource_packs`.
    pub resource_packs: Vec<crate::resource_pack::ResourcePack>,
    pub public_ip_endpoints: Option<Vec<crate::public_ip::Endpoint>>,

    /// Pages and APIs features add to the web admin panel. See
    /// `tuff_web_ui::panel`.
    pub panel: crate::panel_host::panel::Panel,

    /// Permission nodes core and features check; see `crate::permission`.
    pub permissions: crate::permission::NodeRegistry,

    /// Custom items, block claims, entities and pack assets; validated by
    /// `crate::feature::load_all` into `Shared::custom`.
    pub custom: crate::custom::Definitions,

    /// Feature-owned registries whose value type core does not name.
    /// See [`AnyRegistries`].
    pub ext: AnyRegistries,

    /// Tickers for modeled block entities, by container kind. A kind
    /// without one keeps the built-in tick. See `crate::world::block_entity`.
    pub block_entity_tickers: crate::world::block_entity::BlockEntityTickers,

    /// Which feature registered each hook; filled by `feature::load_all`.
    pub hook_owners: crate::feature::HookOwners,
}

impl Registries {
    pub fn new() -> Self {
        Self {
            block_behaviors: HookSlot::new("block_behaviors"),
            growth_behaviors: HookSlot::new("growth_behaviors"),
            station_rules: HookSlot::new("station_rules"),
            block_rules: HookSlot::new("block_rules"),
            block_loot: HookSlot::new("block_loot"),
            tnt: HookSlot::new("tnt"),
            explosion_rules: HookSlot::new("explosion_rules"),
            dispensers: HookSlot::new("dispensers"),
            block_entity_carrier: HookSlot::new("block_entity_carrier"),
            item_ports: BlockTable::new("item_ports"),
            region_hooks: Vec::new(),
            session_hooks: Vec::new(),
            packet_interceptors: Vec::new(),
            login_gates: Vec::new(),
            save_normalisers: Vec::new(),
            services: Vec::new(),
            player_damage: HookSlot::new("player_damage"),
            fake_motion: HookSlot::new("fake_motion"),
            anti_xray: HookSlot::new("anti_xray"),
            recipe_entries: crate::world::crafting::Registry::default(),
            data_recipes: HookSlot::new("data_recipes"),
            generators: HookSlot::new("generators"),
            dimension_providers: Vec::new(),
            content_providers: Vec::new(),
            fluid_behaviors: HookSlot::new("fluid_behaviors"),
            commands: crate::world::commands::Registry::default(),
            entity_types: crate::world::entity::Registry::default(),
            advancements: HookSlot::new("advancements"),
            advancement_triggers: Registry::new("advancement_triggers"),
            advancement_packs: Registry::new("advancement_packs"),
            advancement_definitions: Registry::new("advancement_definitions"),
            player_saved: Registry::new("player_saved"),
            config_keys: Vec::new(),
            http_routes: Vec::new(),
            resource_packs: Vec::new(),
            public_ip_endpoints: None,
            panel: crate::panel_host::panel::Panel::default(),
            permissions: crate::permission::NodeRegistry::new(),
            custom: crate::custom::Definitions::default(),
            ext: AnyRegistries::default(),
            block_entity_tickers: Default::default(),
            hook_owners: Default::default(),
        }
    }

    /// Called once by [`crate::feature::load_all`] after every feature's
    /// `Resolve` call returns. Freezes every registry this struct owns
    /// directly; `ext` registries are frozen individually by whoever
    /// reads them at server-assembly time (core cannot iterate `ext`'s
    /// contents generically without knowing their types).
    pub fn freeze_all(&mut self) {
        self.block_behaviors.freeze();
        self.growth_behaviors.freeze();
        self.station_rules.freeze();
        self.block_rules.freeze();
        self.block_loot.freeze();
        self.tnt.freeze();
        self.explosion_rules.freeze();
        self.dispensers.freeze();
        self.block_entity_carrier.freeze();
        self.block_entity_tickers.freeze();
        self.item_ports.freeze();
        self.recipe_entries.freeze();
        self.data_recipes.freeze();
        self.generators.freeze();
        self.fluid_behaviors.freeze();
        self.player_damage.freeze();
        self.fake_motion.freeze();
        self.anti_xray.freeze();
        self.advancements.freeze();
        self.advancement_triggers.freeze();
        self.advancement_packs.freeze();
        self.advancement_definitions.freeze();
        self.player_saved.freeze();
        self.panel.freeze();
        self.permissions.register_commands(self.commands.roots());
        self.permissions.freeze();
        for id in self.entity_types.ids() {
            if <tuff_vanilla_data::registry::EntityType as tuff_vanilla_data::Registry>::from_name(
                id,
            )
            .is_none()
            {
                self.commands.add_resource("minecraft:entity_type", id);
            }
        }
        for (name, _) in self.recipe_entries.iter() {
            if tuff_vanilla_data::recipe::RecipeEntry::get(name).is_none() {
                self.commands.add_resource("minecraft:recipe", &**name);
            }
        }
        if let Some(tree) = self.advancements.get() {
            for name in tree.names() {
                if tuff_vanilla_data::advancement::Advancement::get(name).is_none() {
                    self.commands.add_resource("minecraft:advancement", name);
                }
            }
        }
    }
}

impl std::fmt::Debug for Registries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registries").finish_non_exhaustive()
    }
}

impl Default for Registries {
    fn default() -> Self {
        Self::new()
    }
}
