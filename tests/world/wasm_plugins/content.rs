//! Custom content runtime plugins declare at startup: what the owner
//! granted joins the world's content, recipes, advancements and command
//! arguments, and stays until the next start.

use super::*;
use crate::world::crafting::CraftingInput;
use tuff_item::item::{ItemStack, MintReason};
use tuff_plugin_wasm::wit::bindings::types::{
    AssetDecl, AssetFile, ContentDecl, DataFile, ItemDecl,
};
use tuff_vanilla_data::Item;

fn gems() -> Vec<u8> {
    let decl = ContentDecl {
        items: vec![ItemDecl {
            id: "gems:ruby".into(),
            base: "minecraft:emerald".into(),
            model: Some("gems:ruby".into()),
            components: String::new(),
        }],
        entities: Vec::new(),
        assets: vec![AssetDecl {
            name: "items".into(),
            namespace: "gems".into(),
            files: vec![AssetFile {
                path: "items/ruby.json".into(),
                data: br#"{"model":{"type":"minecraft:model","model":"gems:item/ruby"}}"#.to_vec(),
            }],
        }],
        recipes: vec![DataFile {
            id: "gems:ruby_block".into(),
            json: r#"{"type":"minecraft:crafting_shapeless",
                "ingredients":["gems:ruby","gems:ruby","minecraft:redstone"],
                "result":{"id":"minecraft:redstone_block"}}"#
                .into(),
        }],
        advancements: vec![DataFile {
            id: "gems:found_ruby".into(),
            json: r#"{"criteria":{"has":{"trigger":"minecraft:inventory_changed",
                "conditions":{"items":[{"items":"minecraft:emerald"}]}}},
                "rewards":{"recipes":["gems:ruby_block"]}}"#
                .into(),
        }],
    };
    let mut data = Data::new();
    let declared = data.export_result("declare-content", &decl.to_value());
    module(
        "id = gems\nversion = 1.0.0\napi = 0\ncap.content = gems -- adds rubies",
        &format!(
            "(func {} {}) (func {} (i32.const {declared})) {}",
            export("register"),
            ok_unit(),
            export("declare-content"),
            data.wat()
        ),
    )
}

async fn start_gems(grant: Option<&str>) -> (TestDir, TestWorld) {
    let dir = TestDir::new();
    install(dir.path(), "gems.wasm", &gems());
    let backend = open_sqlite(dir.path());
    if let Some(grant) = grant {
        backend
            .config()
            .unwrap()
            .write_plugin_grant("gems", Some(grant))
            .unwrap();
    }
    let world = start_sqlite(dir.path(), backend).await;
    (dir, world)
}

fn shapeless(stacks: Vec<ItemStack>) -> CraftingInput {
    let mut slots: Vec<Option<ItemStack>> = stacks.into_iter().map(Some).collect();
    slots.resize_with(9, || None);
    CraftingInput::positioned(3, 3, &slots).input
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn granted_plugin_content_joins_the_world_at_start() {
    let (_dir, world) = start_gems(Some("content = gems")).await;
    let custom = &world.shared.custom;
    let ruby = custom.item("gems:ruby").expect("the item is registered");
    assert_eq!(ruby.base, Item::Emerald);
    assert!(custom.assets().contains_key("assets/gems/items/ruby.json"));

    let ruby = ruby.stack(1);
    let recipes = world.shared.recipes();
    let input = shapeless(vec![
        ruby.dup_unchecked(),
        ItemStack::mint(Item::Redstone, 1, MintReason::Test),
        ruby.dup_unchecked(),
    ]);
    let (name, recipe) = recipes.find(&input).expect("the recipe crafts");
    assert_eq!(&**name, "gems:ruby_block");
    assert_eq!(recipe.assemble(&input).unwrap().item, Item::RedstoneBlock);
    let emeralds = shapeless(vec![
        ItemStack::mint(Item::Emerald, 1, MintReason::Test),
        ItemStack::mint(Item::Redstone, 1, MintReason::Test),
        ItemStack::mint(Item::Emerald, 1, MintReason::Test),
    ]);
    assert!(
        recipes.find(&emeralds).is_none(),
        "plain emeralds are no rubies"
    );
    assert!(world.shared.advancements().id("gems:found_ruby").is_some());

    let resources = world.shared.commands.roots().resources();
    assert!(resources.custom_item("gems:ruby").is_some());
    assert!(resources.contains("minecraft:recipe", "gems:ruby_block"));
    assert!(resources.contains("minecraft:advancement", "gems:found_ruby"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ungranted_plugin_content_is_left_out() {
    let (_dir, world) = start_gems(None).await;
    assert!(world.shared.custom.item("gems:ruby").is_none());
    assert!(world.shared.recipes().get("gems:ruby_block").is_none());
    assert!(world.shared.advancements().id("gems:found_ruby").is_none());
    world.shutdown().await;
}

/// The trigger the awarder's advancement names; without vanilla's
/// survival plugin nothing else registers one.
struct Cutting;

impl crate::feature::Feature for Cutting {
    fn id(&self) -> &'static str {
        "cutting"
    }

    fn load(&self, cx: &mut crate::feature::LoadContext) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() != tuff_item::load::Phase::Content {
            return Ok(());
        }
        let id = cx.feature_id();
        let handler: crate::world::advancement::trigger::Handler =
            std::sync::Arc::new(|_, _, _| false);
        cx.registries()
            .advancement_triggers
            .register(id, "gems:cut", handler)
    }
}

/// Grants the plugin's advancement, tries one outside its namespaces,
/// then revokes it again.
fn awarder() -> Vec<u8> {
    let decl = ContentDecl {
        items: Vec::new(),
        entities: Vec::new(),
        assets: Vec::new(),
        recipes: Vec::new(),
        advancements: vec![DataFile {
            id: "gems:found_ruby".into(),
            json: r#"{"criteria":{"has":{"trigger":"gems:cut"}}}"#.into(),
        }],
    };
    let mut data = Data::new();
    let declared = data.export_result("declare-content", &decl.to_value());
    let who = || tuff_plugin_wasm::sandbox::uuid(77).to_value().into_owned();
    let none = || WitValue::Variant(0, None);
    let grant = data.call("advancement-grant", &[who(), s("gems:found_ruby"), none()]);
    let outside = data.call(
        "advancement-grant",
        &[who(), s("minecraft:story/root"), none()],
    );
    let revoke = data.call("advancement-revoke", &[who(), s("gems:found_ruby"), none()]);
    let register = register(&mut data, &["award"], "");
    module(
        "id = gems\nversion = 1.0.0\napi = 0\ncap.content = gems\ncap.player-write = all",
        &format!(
            "{register} (func {} (local $a i32) {grant} (local.set $a (i32.add (i32.load8_u (i32.const {RET})) (i32.mul (i32.load8_u offset=1 (i32.const {RET})) (i32.const 10)))) {outside} {revoke} {}) (func {} (i32.const {declared})) {}",
            export("run-command"),
            ok_code("(local.get $a)"),
            export("declare-content"),
            data.wat()
        ),
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_advancement_imports_reach_the_player_without_survival() {
    use crate::world::advancement::SessionAdvancements;
    use crate::world::messages::{Directive, DirectiveSource};
    let dir = TestDir::new();
    install(dir.path(), "gems.wasm", &awarder());
    let backend = open_sqlite(dir.path());
    backend
        .config()
        .unwrap()
        .write_plugin_grant("gems", Some("content = gems\nplayer-write = all"))
        .unwrap();
    let plugin = WasmPluginsFeature {
        directory: Some(plugins_dir(dir.path())),
        limits: Limits {
            fuel: 2_000_000,
            ..Limits::default()
        },
    };
    let world =
        TestWorld::start_without_survival_on(backend, vec![Box::new(Cutting), Box::new(plugin)]);
    host(&world).sync().await;
    let outcome = host(&world)
        .command(Player::new(77, "Steve"), "award".into())
        .await
        .expect("the plugin owns /award");
    assert_eq!(i64::from(outcome.result.expect("runs")), 0);
    host(&world).sync().await;
    world.ticks(2).await;
    let queued = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let queued = world.plugin_advancements.lock().unwrap().clone();
            if queued.len() >= 2 && world.shared.advancements().id("gems:found_ruby").is_some() {
                return queued;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the plugin's advancement grants and tree reach the world");
    assert_eq!(
        queued,
        [
            (77, true, "gems:found_ruby".to_owned(), None),
            (77, false, "gems:found_ruby".to_owned(), None),
        ]
    );

    let tree = world.shared.advancements();
    assert!(tree.id("gems:found_ruby").is_some(), "{}", tree.len());
    let (mut player, mut outbound) = session_with_outbound(&world, 9);
    player.stand_at([0.5, 100.0, 0.5], true);
    let tree = player.progress.advancement_tree.clone();
    let id = tree
        .id("gems:found_ruby")
        .expect("the plugin's tree is live");
    let source = || DirectiveSource {
        name: TextComponent::new("Plugin"),
        player: None,
    };
    let directive = |grant| Directive::Advancement {
        grant,
        tree: tree.clone(),
        ids: vec![id],
        criterion: None,
        show: grant,
    };
    for (_, grant, _, _) in queued {
        player.apply_directive(directive(grant), source()).await;
        player.run_tick().await;
        assert_eq!(player.advancement_done("gems:found_ruby"), grant);
    }
    let packet = tuff_vanilla_data::packet::play::Clientbound::UpdateAdvancements.id();
    let mut updates = 0;
    while let Ok(frame) = outbound.try_recv() {
        if framed_payload(&frame).read_var_int().unwrap() == packet {
            updates += 1;
        }
    }
    assert!(updates >= 2);
    world.shutdown().await;
}
