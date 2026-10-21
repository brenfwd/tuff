use super::*;
use crate::world::crafting::CraftingInput;
use bytes::Bytes;
use tuff_item::item::{ItemStack, MintReason};

fn base() -> Definitions {
    let mut defs = Definitions::default();
    defs.register_item("built-in", CustomItemDef::new("core:gem", Item::Diamond))
        .unwrap();
    defs.resolve().unwrap();
    defs
}

fn ruby(owner: &str) -> ProvidedContent {
    ProvidedContent {
        owner: owner.into(),
        items: vec![CustomItemDef::new("mine:ruby", Item::Emerald).with_model("mine:ruby")],
        entities: vec![CustomEntityDef::new(
            "mine:golem",
            stand_in("iron_golem").unwrap(),
        )],
        assets: vec![(
            "textures".into(),
            AssetSet {
                namespace: "mine".into(),
                files: vec![("textures/item/ruby.png".into(), Bytes::from_static(b"png"))],
            },
        )],
        ..Default::default()
    }
}

fn grid(slots: Vec<Option<ItemStack>>) -> CraftingInput<'static> {
    CraftingInput::positioned(3, 3, slots.leak()).input
}

#[tuff_core::watchdog]
#[test]
fn provided_content_joins_the_features_content() {
    let defs = base();
    let (content, accepted) = defs.with_provided(vec![ruby("rubies")]);
    assert_eq!(accepted.len(), 1);
    assert!(content.item("core:gem").is_some());
    let ruby = content.item("mine:ruby").unwrap();
    assert_eq!(ruby.stack(1).item, Item::Emerald);
    assert!(content.entity("mine:golem").is_some());
    assert!(
        content
            .assets()
            .contains_key("assets/mine/textures/item/ruby.png")
    );
    assert_eq!(content.asset_sets()[0].feature, "rubies");
    assert!(defs.content().item("mine:ruby").is_none());
}

#[tuff_core::watchdog]
#[test]
fn a_clashing_or_invalid_owner_is_left_out_whole() {
    let defs = base();
    let mut clash = ruby("thief");
    clash
        .items
        .push(CustomItemDef::new("core:gem", Item::Stick));
    let dangling = ProvidedContent {
        owner: "dangling".into(),
        items: vec![CustomItemDef::new("odd", Item::Stick)],
        ..Default::default()
    };
    let (content, accepted) = defs.with_provided(vec![clash, dangling, ruby("rubies")]);
    let owners: Vec<_> = accepted.iter().map(|c| c.owner.as_str()).collect();
    assert_eq!(owners, ["rubies"]);
    assert_eq!(content.item("core:gem").unwrap().base, Item::Diamond);
    assert!(content.item("odd").is_none());
    assert!(content.item("mine:ruby").is_some());
}

#[tuff_core::watchdog]
#[test]
fn recipes_name_custom_items_and_advancements_join_the_tree() {
    let defs = base();
    let mut content = ruby("rubies");
    content.recipes = vec![
        DataFile {
            id: "mine:ruby_block".into(),
            json: r#"{"type":"minecraft:crafting_shaped","category":"building",
                "pattern":["RR","RR"],"key":{"R":"mine:ruby"},
                "result":{"id":"mine:ruby","count":2}}"#
                .into(),
        },
        DataFile {
            id: "mine:ruby".into(),
            json: r##"{"type":"crafting_shapeless","ingredients":["#minecraft:planks","core:gem"],
                "result":{"id":"minecraft:stick","count":3}}"##
                .into(),
        },
        DataFile {
            id: "mine:broken".into(),
            json: "{".into(),
        },
        DataFile {
            id: "mine:smelt".into(),
            json: r#"{"type":"smelting","ingredient":"stone","result":{"id":"dirt"}}"#.into(),
        },
    ];
    content.advancements = vec![
        DataFile {
            id: "mine:found_ruby".into(),
            json: r#"{"criteria":{"never":{"trigger":"minecraft:impossible"}},
                "rewards":{"recipes":["mine:ruby_block"]}}"#
                .into(),
        },
        DataFile {
            id: "mine:bad".into(),
            json: r#"{"criteria":{"x":{"trigger":"mine:no_such_trigger"}}}"#.into(),
        },
    ];
    let tree: Arc<dyn Advancements> = Arc::new(crate::world::advancement::NoAdvancements);
    let recipes = Arc::new(RecipeSet::default());
    let merged = merge(&defs, &recipes, &tree, None, vec![content]);
    let ruby = merged.custom.item("mine:ruby").unwrap().stack(1);
    let mut slots = (0..9).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    for slot in [0, 1, 3, 4] {
        slots[slot] = Some(ruby.test_copy());
    }
    let (name, recipe) = merged.recipes.find(&grid(slots)).unwrap();
    assert_eq!(&**name, "mine:ruby_block");
    let made = recipe
        .assemble(&grid(
            (0..9).map(|_| None).collect::<Vec<Option<ItemStack>>>(),
        ))
        .unwrap();
    assert_eq!(
        (made.count(), crate::custom::item_id(&made)),
        (2, Some("mine:ruby"))
    );
    assert_eq!(
        recipe.category(),
        RecipeBookCategory::CraftingBuildingBlocks
    );

    let gem = merged.custom.item("core:gem").unwrap().stack(1);
    let plain = ItemStack::mint(Item::OakPlanks, 1, MintReason::Test);
    let mut slots = (0..9).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    slots[4] = Some(plain.test_copy());
    slots[8] = Some(gem);
    let (name, _) = merged.recipes.find(&grid(slots)).unwrap();
    assert_eq!(&**name, "mine:ruby");
    let mut slots = (0..9).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    slots[4] = Some(plain);
    slots[8] = Some(ItemStack::mint(Item::Diamond, 1, MintReason::Test));
    assert!(merged.recipes.find(&grid(slots)).is_none());

    assert!(merged.recipes.get("mine:broken").is_none());
    assert!(
        merged.recipes.get("mine:smelt").is_none(),
        "no data recipe factory"
    );
    assert!(Arc::ptr_eq(merged.recipes.custom(), &merged.custom));
}

#[tuff_core::watchdog]
#[test]
fn nothing_provided_keeps_everything_as_is() {
    let defs = base();
    let tree: Arc<dyn Advancements> = Arc::new(crate::world::advancement::NoAdvancements);
    let recipes = Arc::new(RecipeSet::default().with_custom(defs.content()));
    let merged = merge(&defs, &recipes, &tree, None, Vec::new());
    assert!(Arc::ptr_eq(&merged.recipes, &recipes));
    assert!(Arc::ptr_eq(&merged.advancements, &tree));
    assert!(Arc::ptr_eq(&merged.custom, &defs.content()));
}
