//! A small feature that uses each surface core offers features, as a worked
//! example and so those surfaces stay usable: a world service, a region
//! syscall hook, session hooks with per-player state, a `PlayerSession`
//! extension trait, a command, an `ext` registry, an entity type under its
//! own id, an advancement trigger with an advancement using it, and two
//! recipes of its own (a grid recipe and a code recipe) beside the vanilla ones, a custom item,
//! and a web panel page with its API.
//!
//! What it does: it counts the gold blocks players place in a world,
//! remembers how long each player has sneaked, answers `/gold` and the chat
//! keyword `!gold` with both (firing its `example:gold_report` trigger),
//! greets players by a registered phrase, adds `example:tick_counter`,
//! an entity that counts and saves its own ticks, crafts dirt into
//! diamonds and repairs damaged tools with a diamond. Its panel
//! page shows the gold count live and resets it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tuff_vanilla_data::command::{CommandNode, CommandNodeKind};
use tuff_vanilla_data::recipe::Ingredient;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, Item};

use tuff_core::command::RootCommand;
use tuff_core::custom::{AssetSet, CustomItemDef};
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::metrics::{Kind, Series};
use tuff_core::protocol::play::Serverbound;
use tuff_core::protocol::recipe::{RecipeDisplay, SlotDisplay};
use tuff_core::registry::Registries;
use tuff_core::world::Shared;
use tuff_core::world::advancement::Definition;
use tuff_core::world::advancement::trigger::{Event, Handler};
use tuff_core::world::commands::{Context, Executor, ExecutorFuture, executor_future};
use tuff_core::world::crafting::{CraftingInput, Placement, Recipe, register_unlocked};
use tuff_core::world::dimension::Dimensions;
use tuff_core::world::entity::{EntityBase, RegionEntity, Tick, TickContext, take_number};
use tuff_core::world::predicate::EntityFacts;
use tuff_core::world::region_api::{BlockAction, PlayerBlockAction, RegionHooks, RegionWorld};
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{HookFuture, SessionHooks, SessionTick};
use tuff_http::Response;
use tuff_item::item::{ItemStack, MintReason};
use tuff_item::load::{LoadError, Phase};
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_web_ui::Scope;
use tuff_web_ui::audit::Entry;
use tuff_web_ui::events::PanelEvents;
use tuff_web_ui::panel::{
    ApiCall, Asset, Method, MetricCard, PanelApi, PanelPage, StatusCode, api_handler,
};

pub const ID: &str = "example";

/// The example's always-on features; they compose with the vanilla set.
pub struct ExampleFeature;

impl Feature for ExampleFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        match cx.phase() {
            Phase::Types => {
                cx.registries()
                    .ext
                    .registry::<Greeting>("example:greetings");
            }
            Phase::Content => {
                let registries = cx.registries();
                registries
                    .ext
                    .registry::<Greeting>("example:greetings")
                    .register(ID, "example:hello", Greeting("Welcome, gold digger"))?;
                registries.services.push(Arc::new(GoldCounterFactory));
                registries.region_hooks.push(Arc::new(GoldHooks));
                registries.session_hooks.push(Arc::new(SneakHooks));
                registries.commands.register(
                    RootCommand {
                        name: "gold",
                        node: &GOLD_NODE,
                        providers: &[],
                    },
                    GoldCommand,
                );
                registries.entity_types.register_as(
                    TICK_COUNTER_ID,
                    EntityType::Interaction,
                    load_tick_counter,
                );
                let handler: Handler = Arc::new(gold_report_matches);
                registries
                    .advancement_triggers
                    .register(ID, GOLD_REPORT_TRIGGER, handler)?;
                registries
                    .advancement_definitions
                    .register(ID, GOLD_DIGGER.name, GOLD_DIGGER)?;
                DirtToDiamond::register(registries)?;
                registries
                    .recipe_entries
                    .register(ID, DIAMOND_REPAIR, DiamondRepair)?;
                register_custom(registries)?;
                register_panel(registries)?;
            }
            _ => {}
        }
        Ok(())
    }
}

pub const GOLD_NUGGET_ITEM: &str = "example:gold_nugget_charm";

fn register_custom(registries: &mut Registries) -> Result<(), LoadError> {
    let custom = &mut registries.custom;
    custom.register_item(
        ID,
        CustomItemDef::new(GOLD_NUGGET_ITEM, Item::GoldNugget).with_model(GOLD_NUGGET_ITEM),
    )?;
    custom.register_assets(
        ID,
        "example:charm",
        AssetSet {
            namespace: "example".to_owned(),
            files: vec![(
                "items/gold_nugget_charm.json".to_owned(),
                bytes::Bytes::from_static(
                    br#"{"model":{"type":"minecraft:model","model":"minecraft:item/gold_nugget"}}"#,
                ),
            )],
        },
    )
}

// ---- web panel page and API ----------------------------------------------

/// The page and API id: the page is served under `/admin/pages/example-gold/`
/// and the API at `/admin/api/v1/plugins/example-gold`.
pub const GOLD_PAGE: &str = "example-gold";
/// Seeing the page and reading the count.
pub const GOLD_READ: Scope = Scope::Plugin("example.gold.read");
/// Resetting the count, checked by the API handler itself.
pub const GOLD_RESET: Scope = Scope::Plugin("example.gold.reset");
/// The event topic the count is published on after a reset.
pub const GOLD_TOPIC: &str = "plugin.example-gold.count";
/// The metrics series (a gauge) holding the count, shown as a dashboard
/// card to sessions that may read it.
pub const GOLD_SERIES: &str = "example.gold_placed";

fn register_panel(registries: &mut Registries) -> Result<(), LoadError> {
    let module = Asset::module(
        "gold.js",
        include_str!(concat!(env!("OUT_DIR"), "/panel/gold.js")),
    );
    let style = Asset::stylesheet(
        "gold.css",
        include_str!(concat!(env!("OUT_DIR"), "/panel/gold.css")),
    );
    registries.panel.register_page(
        ID,
        PanelPage::new(GOLD_PAGE, "Gold", GOLD_READ, module)
            .style(style)
            .icon("coins"),
    )?;
    registries.panel.register_api(
        ID,
        PanelApi::new(GOLD_PAGE, GOLD_READ, api_handler(gold_api)),
    )?;
    registries.panel.register_card(
        ID,
        MetricCard::new(GOLD_SERIES, "Gold placed").scope(GOLD_READ),
    )
}

/// `GET /` answers the count; `POST /reset` zeroes it, audits the reset and
/// publishes the new count to open pages.
async fn gold_api(call: ApiCall) -> Response {
    let Some(shared) = call.server::<Shared>() else {
        return Response::status(StatusCode::SERVICE_UNAVAILABLE);
    };
    let ApiCall { auth, request, .. } = call;
    match (&request.method, request.rest.as_str()) {
        (&Method::GET, "" | "/") => Response::json(
            StatusCode::OK,
            &serde_json::json!({ "goldPlaced": gold_placed(&shared) }),
        ),
        (&Method::POST, "/reset") => {
            if let Err(resp) = auth.require(GOLD_RESET) {
                return resp;
            }
            if let Some(counter) = shared.services.get::<GoldCounter>() {
                counter.reset();
            }
            shared.web.audit.record(
                Entry::new(auth.principal.name(), "example.gold.reset").remote(request.remote.ip()),
            );
            let body = serde_json::json!({ "goldPlaced": gold_placed(&shared) });
            Response::json(StatusCode::OK, &body)
        }
        _ => Response::json(
            StatusCode::NOT_FOUND,
            &serde_json::json!({ "error": "no such endpoint" }),
        ),
    }
}

/// A phrase players are greeted with, in the feature's own registry.
pub struct Greeting(pub &'static str);

/// The registered greetings, as other features would look them up.
pub fn greetings(registries: &mut Registries) -> &mut tuff_item::registry::Registry<Greeting> {
    registries.ext.registry::<Greeting>("example:greetings")
}

// ---- world service -----------------------------------------------------

/// How often the gold count is pushed to open panel pages at most, so a
/// burst of placements coalesces into a few publishes per second instead of
/// flooding the event bus.
const GOLD_PUBLISH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Gold blocks players placed in this world, kept for the world's life.
#[derive(Default)]
pub struct GoldCounter {
    count: AtomicU64,
    series: std::sync::OnceLock<Series>,
    events: std::sync::OnceLock<PanelEvents>,
    last_published: std::sync::Mutex<Option<std::time::Instant>>,
}

impl GoldCounter {
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    fn add(&self) {
        let count = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(series) = self.series.get() {
            series.set(count as i64);
        }
        self.publish(count, false);
    }

    fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
        if let Some(series) = self.series.get() {
            series.set(0);
        }
        self.publish(0, true);
    }

    /// Pushes the count to open panel pages, from the non-blocking panel
    /// event bus. Safe to call from a synchronous region hook: it never
    /// awaits or blocks. Unless `force`, this coalesces to at most one
    /// publish per `GOLD_PUBLISH_INTERVAL` so rapid placements do not flood
    /// the event bus.
    fn publish(&self, count: u64, force: bool) {
        let Some(events) = self.events.get() else {
            return;
        };
        let now = std::time::Instant::now();
        {
            let mut last = self.last_published.lock().expect("lock poisoned");
            if !force && last.is_some_and(|at| now.duration_since(at) < GOLD_PUBLISH_INTERVAL) {
                return;
            }
            *last = Some(now);
        }
        let body = serde_json::json!({ "goldPlaced": count }).to_string();
        events.publish(GOLD_TOPIC, "count", body);
    }
}

impl WorldService for GoldCounter {
    fn start(&self, shared: &Arc<Shared>) {
        let series = shared.metrics.series(GOLD_SERIES, Kind::Gauge);
        series.set(self.count() as i64);
        let _ = self.series.set(series);
        let _ = self.events.set(shared.web.events.clone());
    }
}

struct GoldCounterFactory;

impl ServiceFactory for GoldCounterFactory {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(GoldCounter::default())
    }
}

/// The world's gold count, 0 when the service does not run.
pub fn gold_placed(shared: &Shared) -> u64 {
    shared
        .services
        .get::<GoldCounter>()
        .map_or(0, GoldCounter::count)
}

// ---- region syscall hook -------------------------------------------------

struct GoldHooks;

impl RegionHooks for GoldHooks {
    fn player_block_action(&self, world: &mut dyn RegionWorld, action: &PlayerBlockAction) {
        if action.action == BlockAction::Placed
            && action.state.block() == Block::GoldBlock
            && let Some(counter) = world.shared().services.get::<GoldCounter>()
        {
            counter.add();
        }
    }
}

// ---- session hooks and per-player state ----------------------------------

/// Ticks a player has spent sneaking, in the session's extensions.
#[derive(Default)]
pub struct SneakTime(pub u64);

struct SneakHooks;

impl SessionHooks for SneakHooks {
    fn tick<'a>(&'a self, session: &'a mut PlayerSession, phase: SessionTick) -> HookFuture<'a> {
        if phase == SessionTick::Late && session.pose.sneaking {
            session.count_sneak_tick();
        }
        Box::pin(async {})
    }

    fn packet<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a mut Option<Serverbound>,
    ) -> HookFuture<'a> {
        if matches!(packet, Some(Serverbound::Chat(message)) if message == "!gold") {
            *packet = None;
            session.report_gold();
        }
        Box::pin(async {})
    }
}

// ---- PlayerSession extension trait ------------------------------------------

/// What this feature adds to a session. Features cannot write inherent
/// `impl PlayerSession` blocks, so they implement their own trait over its
/// `pub` fields and methods; names must not clash with core's inherent
/// methods, which would silently win.
pub trait GoldDiggerExt {
    fn count_sneak_tick(&mut self);
    fn sneak_ticks(&self) -> u64;
    /// Tells the player the gold count and fires `example:gold_report`.
    fn report_gold(&mut self);
}

impl GoldDiggerExt for PlayerSession {
    fn count_sneak_tick(&mut self) {
        self.ext.entry::<SneakTime>().0 += 1;
    }

    fn sneak_ticks(&self) -> u64 {
        self.ext.get::<SneakTime>().map_or(0, |time| time.0)
    }

    fn report_gold(&mut self) {
        let text = report(&self.shared, self.sneak_ticks());
        self.system_message(&text);
        let report = GoldReport {
            gold_placed: gold_placed(&self.shared),
        };
        self.fire(Event::Custom {
            trigger: GOLD_REPORT_TRIGGER,
            data: &report,
        });
    }
}

// ---- advancement trigger ---------------------------------------------------

/// The trigger id; a criterion `{"trigger": "example:gold_report",
/// "conditions": {"min_gold": 3}}` is met by a report of at least 3.
pub const GOLD_REPORT_TRIGGER: &str = "example:gold_report";

/// Met by a gold report of at least one block, under the story tab.
pub const GOLD_DIGGER: Definition = Definition {
    name: "example:gold_digger",
    parent: Some("minecraft:story/root"),
    display: None,
    criteria: &[tuff_vanilla_data::advancement::Criterion {
        name: "reported",
        trigger: GOLD_REPORT_TRIGGER,
        conditions: Some(r#"{"min_gold":1}"#),
    }],
    requirements: &[&["reported"]],
    rewards: tuff_vanilla_data::advancement::Rewards::NONE,
    sends_telemetry_event: false,
    position: [0.0, 0.0],
};

/// The data an `example:gold_report` event carries.
pub struct GoldReport {
    pub gold_placed: u64,
}

/// The trigger's handler: `min_gold` (default 0) against the report.
pub fn gold_report_matches(conditions: &Value, event: &Event<'_>, _player: &EntityFacts) -> bool {
    let Event::Custom { data, .. } = event else {
        return false;
    };
    let Some(report) = data.downcast_ref::<GoldReport>() else {
        return false;
    };
    let min = conditions
        .get("min_gold")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    report.gold_placed >= min
}

fn report(shared: &Shared, sneak_ticks: u64) -> TextComponent {
    TextComponent::new(format!(
        "{} gold blocks placed; you have sneaked for {} ticks",
        gold_placed(shared),
        sneak_ticks
    ))
}

// ---- command ---------------------------------------------------------------

static GOLD_NODE: CommandNode = CommandNode {
    name: "gold",
    kind: CommandNodeKind::Literal,
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};

struct GoldCommand;

impl Executor for GoldCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            let sneaked = ctx
                .actor
                .player()
                .map_or(0, |session| session.sneak_ticks());
            let text = report(ctx.actor.shared(), sneaked);
            ctx.reply(text);
            Ok(gold_placed(ctx.actor.shared()) as i32)
        })
    }
}

// ---- entity type under its own id ------------------------------------------

/// The saved id; clients are sent it as an interaction entity, which is
/// invisible and needs no entity data.
pub const TICK_COUNTER_ID: &str = "example:tick_counter";

/// An entity that counts the ticks it has lived, saved as `ticks`.
#[derive(Debug)]
pub struct TickCounter {
    pub base: EntityBase,
    pub ticks: u64,
}

fn load_tick_counter(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let ticks = take_number(nbt, "ticks").unwrap_or(0.0) as u64;
    Some(Box::new(TickCounter { base, ticks }))
}

impl RegionEntity for TickCounter {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn saved_id(&self) -> &str {
        TICK_COUNTER_ID
    }

    fn save(&self, out: &mut Compound) {
        out.insert("ticks", self.ticks as i64);
    }

    fn display_name(&self) -> String {
        TICK_COUNTER_ID.to_owned()
    }

    fn tick(&mut self, _cx: &mut TickContext) -> Tick {
        self.ticks += 1;
        Tick::Alive
    }
}

// ---- recipes ---------------------------------------------------------------

/// A grid recipe of the feature's own: one dirt anywhere in a crafting grid
/// makes a diamond. It is in the recipe book once the player picks up dirt.
pub struct DirtToDiamond;

pub const DIAMOND_FROM_DIRT: &str = "example:diamond_from_dirt";

impl DirtToDiamond {
    const DIRT: Ingredient = Ingredient::Item(Item::Dirt);

    fn register(registries: &mut Registries) -> Result<(), LoadError> {
        register_unlocked(ID, registries, DIAMOND_FROM_DIRT, Self, &[Self::DIRT])
    }
}

impl Recipe for DirtToDiamond {
    fn matches(&self, input: &CraftingInput) -> bool {
        input.ingredient_count() == 1 && input.stacks().any(|s| s.item == Item::Dirt)
    }

    fn assemble(&self, _input: &CraftingInput) -> Option<ItemStack> {
        Some(ItemStack::mint(Item::Diamond, 1, MintReason::Crafting))
    }

    fn displays(&self) -> Vec<RecipeDisplay> {
        vec![RecipeDisplay::Shapeless {
            ingredients: vec![SlotDisplay::Ingredient(Self::DIRT)],
            result: SlotDisplay::template(ItemStack::mint(Item::Diamond, 1, MintReason::Crafting)),
            station: SlotDisplay::Item(Item::CraftingTable),
        }]
    }

    fn placement(&self) -> Option<Placement> {
        Some(Placement::shapeless(vec![Self::DIRT]))
    }
}

pub const DIAMOND_REPAIR: &str = "example:diamond_repair";

/// A code recipe: any damaged tool with a diamond comes out fully repaired.
/// It has no fixed ingredient list, so the recipe book never lists it.
pub struct DiamondRepair;

impl DiamondRepair {
    fn damaged_tool<'a>(input: &CraftingInput<'a>) -> Option<&'a ItemStack> {
        if input.ingredient_count() != 2 || !input.stacks().any(|s| s.item == Item::Diamond) {
            return None;
        }
        input
            .stacks()
            .find(|stack| stack.item != Item::Diamond && stack.has_damage())
    }
}

impl Recipe for DiamondRepair {
    fn matches(&self, input: &CraftingInput) -> bool {
        Self::damaged_tool(input).is_some()
    }

    fn assemble(&self, input: &CraftingInput) -> Option<ItemStack> {
        let mut repaired = ItemStack::mint_like(Self::damaged_tool(input)?, 1, MintReason::Crafting);
        repaired.set_damage(0);
        Some(repaired)
    }
}
