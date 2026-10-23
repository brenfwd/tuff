//! Custom items at runtime: behaviour dispatch on the session and in
//! regions, suppression of the base item, private data restored for
//! creative clients, command arguments and custom recipes.

use super::*;
use std::sync::Mutex;

use crate::world::inventory::Inventory;

use tuff_core::custom::recipes::CustomShapeless;
use tuff_core::custom::{CustomItemBehavior, CustomItemDef, Handled, items::UseFuture};
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::protocol::codec::CreativeSlot;
use tuff_core::protocol::play::{GameMode, Serverbound};
use tuff_core::world::entity::{EntityInteraction, InteractOutcome};
use tuff_core::world::messages::HandEffect;
use tuff_core::world::region_api::RegionWorld;
use tuff_core::world::session::PlayerSession;
use tuff_item::load::{LoadError, Phase};
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::EntityType;

type Log = Arc<Mutex<Vec<String>>>;

struct Recorder {
    name: &'static str,
    log: Log,
    use_result: Handled,
}

impl CustomItemBehavior for Recorder {
    fn use_item<'a>(&'a self, _session: &'a mut PlayerSession, _off_hand: bool) -> UseFuture<'a> {
        self.log.lock().unwrap().push(format!("{} use", self.name));
        let result = self.use_result;
        Box::pin(async move { result })
    }

    fn use_on_block(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        if state.block() != Block::Stone {
            return None;
        }
        self.log
            .lock()
            .unwrap()
            .push(format!("{} block", self.name));
        world.put_block(place.target, Block::GoldBlock.default_state());
        Some(ActionOutcome::Used {
            off_hand: place.off_hand,
            item: Item::FlintAndSteel,
            effect: HandEffect::Damage(1),
        })
    }

    fn use_on_entity(
        &self,
        _world: &mut dyn RegionWorld,
        _target: i32,
        interaction: &EntityInteraction,
    ) -> InteractOutcome {
        let id = interaction
            .stack
            .as_deref()
            .and_then(tuff_core::custom::item_id)
            .unwrap_or("none");
        self.log
            .lock()
            .unwrap()
            .push(format!("{} entity {id}", self.name));
        Some(HandEffect::Consume(1))
    }
}

struct Items(Log);

const CHARM: &str = "test:apple_charm";
const SWEET: &str = "test:sweet_apple";
const WAND: &str = "test:wand";
const DUD: &str = "test:dud";
const LIGHTER: &str = "test:lighter";

impl Feature for Items {
    fn id(&self) -> &'static str {
        "test-custom-items"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() != Phase::Content {
            return Ok(());
        }
        let recorder = |name, use_result| -> Arc<dyn CustomItemBehavior> {
            Arc::new(Recorder {
                name,
                log: self.0.clone(),
                use_result,
            })
        };
        let registries = cx.registries();
        let charm = CustomItemDef::new(CHARM, Item::Apple)
            .with_model(CHARM)
            .private()
            .with_data(Compound::new().with("secret", 7))
            .with_behavior(recorder("charm", Handled::Pass));
        let custom = &mut registries.custom;
        custom.register_item(self.id(), charm.clone())?;
        custom.register_item(
            self.id(),
            CustomItemDef::new(SWEET, Item::Apple)
                .inheriting()
                .with_behavior(recorder("sweet", Handled::Pass)),
        )?;
        custom.register_item(
            self.id(),
            CustomItemDef::new(WAND, Item::FlintAndSteel)
                .with_behavior(recorder("wand", Handled::Consumed)),
        )?;
        custom.register_item(self.id(), CustomItemDef::new(DUD, Item::FlintAndSteel))?;
        custom.register_item(
            self.id(),
            CustomItemDef::new(LIGHTER, Item::FlintAndSteel).inheriting(),
        )?;
        CustomShapeless::new(
            "test:charm_from_apples",
            vec![Item::Apple.into(), Item::GoldNugget.into()],
            charm.stack(1),
        )
        .register(self.id(), registries)?;
        Ok(())
    }
}

fn start(dir: &TestDir) -> (TestWorld, Log) {
    let log = Log::default();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(Items(log.clone()))]);
    (world, log)
}

fn stack(world: &TestWorld, id: &str, count: u8) -> ItemStack {
    world.shared.custom.stack(id, count).unwrap()
}

async fn result(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

fn taken(log: &Log) -> Vec<String> {
    std::mem::take(&mut *log.lock().unwrap())
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn use_item_runs_the_behaviour_and_suppresses_the_base_unless_inherited() {
    let dir = TestDir::new();
    let (world, log) = start(&dir);
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.make_hungry();
    let eat = Serverbound::UseItem {
        off_hand: false,
        sequence: 1,
    };

    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(stack(&world, CHARM, 1)));
    player.handle_packet(eat.clone()).await;
    assert_eq!(taken(&log), ["charm use"]);
    assert!(!player.is_using(), "the apple base is not eaten");

    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(stack(&world, SWEET, 1)));
    player.handle_packet(eat.clone()).await;
    assert_eq!(taken(&log), ["sweet use"]);
    assert!(player.is_using(), "an inheriting custom apple is eaten");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removed_base_components_stop_the_client_and_server_use() {
    use tuff_vanilla_data::registry::DataComponentType;

    let dir = TestDir::new();
    let (world, log) = start(&dir);
    let charm = stack(&world, CHARM, 1);
    assert_eq!(
        charm.components.network_removed(),
        [DataComponentType::Consumable, DataComponentType::Food],
        "the client is told the custom apple is not food"
    );
    assert!(
        stack(&world, SWEET, 1)
            .components
            .network_removed()
            .is_empty()
    );

    let (mut player, _out) = session_with_outbound(&world, 1);
    player.make_hungry();
    let eat = Serverbound::UseItem {
        off_hand: false,
        sequence: 1,
    };
    let mut plain = ItemStack::mint(Item::Apple, 1, MintReason::Test);
    plain.components.removed = charm.components.removed.clone();
    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(plain));
    player.handle_packet(eat.clone()).await;
    assert!(
        !player.is_using(),
        "an apple without consumable is not eaten"
    );
    assert!(taken(&log).is_empty());

    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::Apple, 1, MintReason::Test)),
    );
    player.handle_packet(eat).await;
    assert!(player.is_using(), "a plain apple is eaten");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn block_clicks_go_to_the_behaviour_and_hide_the_base_item() {
    let dir = TestDir::new();
    let (world, log) = start(&dir);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let stone = BlockPos::new(4, SKY, 4);
    let tnt = BlockPos::new(8, SKY, 8);
    let lit_tnt = BlockPos::new(10, SKY, 10);
    place_blocks(
        &world,
        &region,
        vec![
            (stone, Block::Stone.default_state()),
            (tnt, Block::Tnt.default_state()),
            (lit_tnt, Block::Tnt.default_state()),
        ],
    )
    .await;
    let click = |target, id: &str, sequence| {
        let mut click = place(&player, target, Item::FlintAndSteel, sequence);
        click.components =
            tuff_item::item::ComponentsSnapshot::of(&stack(&world, id, 1).components);
        click
    };

    region
        .request(RegionRequest::Place(click(stone, WAND, 1)))
        .await;
    assert!(matches!(
        result(&mut events, 1).await,
        ActionOutcome::Used {
            effect: HandEffect::Damage(1),
            ..
        }
    ));
    assert_eq!(taken(&log), ["wand block"]);
    assert_eq!(
        block_at(&region, stone).await.map(|s| s.block()),
        Some(Block::GoldBlock)
    );

    region
        .request(RegionRequest::Place(click(tnt, DUD, 2)))
        .await;
    assert!(matches!(
        result(&mut events, 2).await,
        ActionOutcome::Rejected(_)
    ));
    region
        .request(RegionRequest::Place(click(lit_tnt, LIGHTER, 3)))
        .await;
    assert!(!matches!(
        result(&mut events, 3).await,
        ActionOutcome::Rejected(_)
    ));
    world.ticks(2).await;
    assert_eq!(
        block_at(&region, tnt).await.map(|s| s.block()),
        Some(Block::Tnt),
        "a non-inheriting flint and steel lights nothing"
    );
    assert_eq!(
        block_at(&region, lit_tnt).await.map(|s| s.block()),
        Some(Block::Air),
        "an inheriting one lights TNT"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entity_clicks_hide_the_base_and_reach_the_behaviour() {
    let dir = TestDir::new();
    let (world, log) = start(&dir);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Creeper,
        Compound::new().with("NoAI", true),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(2).await;
    let creeper = mobs(&region, EntityType::Creeper).await[0].id;
    let mut click = interaction(&player, None);
    click.stack = Some(tuff_item::item::ItemSnapshot::of(&stack(&world, WAND, 1)));
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: creeper,
            interaction: click,
        })
        .await;
    let effect = expect(&mut events, |event| match event {
        PlayerEvent::EntityInteracted { effect, .. } => Some(effect),
        _ => None,
    })
    .await;
    assert_eq!(effect, HandEffect::Consume(1));
    assert_eq!(taken(&log), [format!("wand entity {WAND}")]);
    world.ticks(40).await;
    assert_eq!(
        mobs(&region, EntityType::Creeper).await.len(),
        1,
        "the creeper was never lit"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creative_clients_get_private_data_back() {
    let dir = TestDir::new();
    let (world, _log) = start(&dir);
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.set_game_mode(GameMode::Creative).await;
    let mut held = stack(&world, CHARM, 3);
    held.components
        .other
        .get_compound_mut("minecraft:custom_data")
        .unwrap()
        .insert("secret", 42);
    player.inventory_mut().set_slot(36, Some(held.test_copy()));
    let client_view = |stack: &ItemStack| {
        let mut view = stack.test_copy();
        for component in stack.components.network_added() {
            if let tuff_item::item::NetworkComponent::ClientData(data) = component {
                view.components
                    .other
                    .insert("minecraft:custom_data", Tag::Compound(data));
            }
        }
        view
    };
    let seen = client_view(&held);
    assert_ne!(seen, held, "the secret is not sent");

    player
        .handle_packet(Serverbound::SetCreativeModeSlot {
            slot: 36,
            stack: CreativeSlot::Empty,
        })
        .await;
    player
        .handle_packet(Serverbound::SetCreativeModeSlot {
            slot: 40,
            stack: CreativeSlot::Stack(tuff_item::item::ItemSnapshot::of(&seen.test_copy())),
        })
        .await;
    assert_eq!(player.inventory().slots()[40].as_ref(), Some(&held));

    let (mut other, _other_out) = session_with_outbound(&world, 2);
    other.set_game_mode(GameMode::Creative).await;
    other
        .handle_packet(Serverbound::SetCreativeModeSlot {
            slot: 41,
            stack: CreativeSlot::Stack(tuff_item::item::ItemSnapshot::of(&client_view(&stack(
                &world, CHARM, 1,
            )))),
        })
        .await;
    let fresh = other.inventory().slots()[41]
        .as_ref()
        .map(tuff_item::item::ItemStack::test_copy)
        .unwrap();
    assert_eq!(
        fresh,
        stack(&world, CHARM, 1),
        "a new stack gets the defaults"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn item_arguments_name_custom_items() {
    use tuff_core::command::args::{ParseContext, Value, parse_argument_with};
    use tuff_core::command::reader::Reader;
    use tuff_core::world::messages::{Directive, DirectiveSource};

    let dir = TestDir::new();
    let (world, _log) = start(&dir);
    let resources = world.shared.commands.roots().resources().clone();
    let parse = |parser: &str, input: &str| {
        parse_argument_with(
            parser,
            None,
            &mut Reader::new(input),
            ParseContext {
                allow_selectors: true,
            },
            &resources,
        )
    };
    let Ok(Value::ItemStack(charm)) = parse("minecraft:item_stack", CHARM) else {
        panic!("{CHARM} parses as an item");
    };
    assert_eq!(charm.stack(2), stack(&world, CHARM, 2));
    let Ok(Value::ItemStack(worn)) = parse(
        "minecraft:item_stack",
        &format!("{WAND}[minecraft:damage=5]"),
    ) else {
        panic!("components apply over a custom item");
    };
    let worn = worn.stack(1);
    assert_eq!(tuff_core::custom::item_id(&worn), Some(WAND));
    assert_eq!(worn.damage(), 5);
    assert!(parse("minecraft:item_stack", "test:unknown").is_err());

    let Ok(Value::ItemPredicate(only_charms)) = parse("minecraft:item_predicate", CHARM) else {
        panic!("{CHARM} parses as a predicate");
    };
    assert!(only_charms.test(&stack(&world, CHARM, 1)));
    assert!(!only_charms.test(&ItemStack::mint(Item::Apple, 1, MintReason::Test)));
    assert!(!only_charms.test(&stack(&world, SWEET, 1)));

    let (mut player, _out) = session_with_outbound(&world, 1);
    let source = DirectiveSource {
        name: tuff_item::text::TextComponent::new("Server"),
        player: None,
    };
    player
        .apply_directive(
            Directive::Give {
                stack: charm.template(1),
                count: 2,
            },
            source,
        )
        .await;
    let given: Vec<ItemStack> = player
        .inventory()
        .slots()
        .iter()
        .flatten()
        .map(tuff_item::item::ItemStack::test_copy)
        .collect();
    assert_eq!(given, [stack(&world, CHARM, 2)]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_inventory_grid_crafts_custom_results_only_from_exact_ingredients() {
    let dir = TestDir::new();
    let (world, _log) = start(&dir);
    let (mut player, _out) = session_with_outbound(&world, 1);
    player
        .inventory_mut()
        .set_slot(1, Some(ItemStack::mint(Item::Apple, 1, MintReason::Test)));
    player.inventory_mut().set_slot(
        2,
        Some(ItemStack::mint(Item::GoldNugget, 1, MintReason::Test)),
    );
    player.refresh_inventory_result();
    assert_eq!(
        player.inventory().slots()[0].as_ref(),
        Some(&stack(&world, CHARM, 1))
    );

    player
        .inventory_mut()
        .set_slot(1, Some(stack(&world, SWEET, 1)));
    player.refresh_inventory_result();
    assert_eq!(
        player.inventory().slots()[0],
        None,
        "custom apples are not apples"
    );
    world.shutdown().await;
}
