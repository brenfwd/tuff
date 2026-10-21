use std::path::Path;

use super::*;
use crate::command::RootCommand;
use tuff_datapack::identifier;
use tuff_storage::test_dir::TestDir;
use tuff_storage::vanilla::VanillaBackend;

fn roots() -> Roots {
    Roots::new(["say", "function", "scoreboard"].map(|name| RootCommand::vanilla(name, &[])))
}

fn meta(description: &str) -> String {
    let current = meta::Format::current().major;
    format!(
        r#"{{"pack":{{"description":"{description}","min_format":{current},"max_format":{current}}}}}"#
    )
}

fn write(root: &Path, path: &str, contents: &[u8]) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn world() -> (TestDir, Arc<dyn WorldBackend>) {
    let dir = TestDir::new();
    let backend: Arc<dyn WorldBackend> = Arc::new(VanillaBackend::new(dir.path().to_path_buf()));
    (dir, backend)
}

/// The ids of the packs other than the built-in feature packs.
fn ids(packs: &[PackInfo]) -> Vec<&str> {
    packs
        .iter()
        .filter(|p| p.source != PackSource::Feature)
        .map(|p| p.id.as_str())
        .collect()
}

fn feature_ids() -> Vec<String> {
    tuff_vanilla_data::version::FEATURE_PACKS
        .iter()
        .map(|p| p.id.to_owned())
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn folders_and_zips_with_metadata_are_packs() {
    let (dir, backend) = world();
    write(
        dir.path(),
        "datapacks/folder/pack.mcmeta",
        meta("a folder").as_bytes(),
    );
    write(
        dir.path(),
        "datapacks/folder/data/a/function/f.mcfunction",
        b"say hi",
    );
    write(
        dir.path(),
        "datapacks/nometa/data/a/function/f.mcfunction",
        b"say hi",
    );
    write(dir.path(), "datapacks/badmeta/pack.mcmeta", b"{not json");
    write(dir.path(), "datapacks/notes.txt", b"hello");
    let zip = zip::write(&[("pack.mcmeta", meta("zipped").as_bytes())]);
    write(dir.path(), "datapacks/zipped.zip", &zip);
    write(dir.path(), "datapacks/broken.zip", b"PK not a zip");
    let packs = discover(&*backend);
    assert_eq!(ids(&packs), ["vanilla", "file/folder", "file/zipped.zip"]);
    assert_eq!(packs[1].meta.description.plain(), "a folder");
    assert_eq!(packs[2].title.plain(), "zipped.zip");
    assert_eq!(packs[1].chat_link(true).plain(), "[file/folder (world)]");
}

#[tuff_core::watchdog]
#[test]
fn a_world_without_a_pack_folder_has_only_vanilla() {
    let (_dir, backend) = world();
    assert_eq!(ids(&discover(&*backend)), ["vanilla"]);
    let memory = crate::storage::Storage::Named(tuff_storage::memory::NAME.into())
        .open(Path::new("unused"))
        .unwrap();
    assert_eq!(ids(&discover(&*memory)), ["vanilla"]);
}

#[tuff_core::watchdog]
#[test]
fn higher_packs_override_functions_and_merge_tags() {
    let (dir, backend) = world();
    write(
        dir.path(),
        "datapacks/low/pack.mcmeta",
        meta("low").as_bytes(),
    );
    write(
        dir.path(),
        "datapacks/low/data/a/function/shared.mcfunction",
        b"say low",
    );
    write(
        dir.path(),
        "datapacks/low/data/a/function/low_only.mcfunction",
        b"say only",
    );
    write(
        dir.path(),
        "datapacks/low/data/minecraft/tags/function/tick.json",
        br#"{"values":["a:low_only"]}"#,
    );
    write(
        dir.path(),
        "datapacks/low/data/minecraft/tags/function/load.json",
        br#"{"values":["a:low_only"]}"#,
    );
    write(dir.path(), "datapacks/low/data/a/recipe/r.json", b"{}");
    let zip = zip::write(&[
        ("pack.mcmeta", meta("high").as_bytes()),
        ("data/a/function/shared.mcfunction", b"say high"),
        ("data/a/function/broken.mcfunction", b"nonsense here"),
        (
            "data/minecraft/tags/function/tick.json",
            br#"{"values":["a:shared"]}"#,
        ),
        (
            "data/minecraft/tags/function/load.json",
            br#"{"replace":true,"values":["a:shared"]}"#,
        ),
        ("data/Bad/function/x.mcfunction", b"say bad namespace"),
        ("data/a/function/../../escape.mcfunction", b"say escape"),
    ]);
    write(dir.path(), "datapacks/zhigh.zip", &zip);
    let packs = discover(&*backend);
    let selected = configure(&packs, &DataPackConfig::default());
    assert_eq!(selected, ["vanilla", "file/low", "file/zhigh.zip"]);
    let contents = load(
        &*backend,
        &roots(),
        &Base::default(),
        &packs,
        &selected,
        7,
        None,
    )
    .unwrap();
    assert_eq!(contents.generation, 7);
    assert_eq!(
        contents.functions.keys().collect::<Vec<_>>(),
        ["a:low_only", "a:shared"]
    );
    let shared = contents.function("a:shared").unwrap();
    assert!(
        matches!(&shared.lines[0], function::Line::Command { input, .. } if input == "say high")
    );
    let tick: Vec<String> = contents
        .tag("minecraft:tick")
        .iter()
        .map(|f| f.id.clone())
        .collect();
    assert_eq!(tick, ["a:low_only", "a:shared"]);
    let load_tag: Vec<String> = contents
        .tag("minecraft:load")
        .iter()
        .map(|f| f.id.clone())
        .collect();
    assert_eq!(load_tag, ["a:shared"]);
}

#[tuff_core::watchdog]
#[test]
fn active_overlays_replace_root_files() {
    let (dir, backend) = world();
    let current = meta::Format::current().major;
    let mcmeta = format!(
        r#"{{"pack":{{"description":"o","min_format":{current},"max_format":{current}}},
           "overlays":{{"entries":[
             {{"directory":"future","min_format":9000,"max_format":9001}},
             {{"directory":"now","min_format":{current},"max_format":{current}}}]}}}}"#
    );
    write(dir.path(), "datapacks/o/pack.mcmeta", mcmeta.as_bytes());
    write(
        dir.path(),
        "datapacks/o/data/a/function/f.mcfunction",
        b"say root",
    );
    write(
        dir.path(),
        "datapacks/o/now/data/a/function/f.mcfunction",
        b"say now",
    );
    write(
        dir.path(),
        "datapacks/o/future/data/a/function/f.mcfunction",
        b"say future",
    );
    write(
        dir.path(),
        "datapacks/o/future/data/a/function/g.mcfunction",
        b"say future",
    );
    let packs = discover(&*backend);
    let selected = configure(&packs, &DataPackConfig::default());
    let contents = load(
        &*backend,
        &roots(),
        &Base::default(),
        &packs,
        &selected,
        1,
        None,
    )
    .unwrap();
    assert_eq!(contents.functions.keys().collect::<Vec<_>>(), ["a:f"]);
    let f = contents.function("a:f").unwrap();
    assert!(matches!(&f.lines[0], function::Line::Command { input, .. } if input == "say now"));
}

#[tuff_core::watchdog]
#[test]
fn saved_selection_is_kept_and_new_packs_join_unless_disabled() {
    let (dir, backend) = world();
    for name in ["a", "b", "c"] {
        write(
            dir.path(),
            &format!("datapacks/{name}/pack.mcmeta"),
            meta(name).as_bytes(),
        );
    }
    let packs = discover(&*backend);
    let config = DataPackConfig {
        enabled: vec!["file/c".into(), "vanilla".into(), "file/gone".into()],
        disabled: vec!["file/a".into(), "feature_pack".into()],
    };
    let selected = configure(&packs, &config);
    assert_eq!(selected, ["file/c", "vanilla", "file/b"]);
    let saved = saved_config(&packs, &selected, &config);
    assert_eq!(saved.enabled, selected);
    let mut disabled = vec!["file/a".to_owned()];
    disabled.extend(feature_ids());
    disabled.push("feature_pack".to_owned());
    assert_eq!(saved.disabled, disabled);
    assert_eq!(
        normalise(&packs, &["file/b".to_owned(), "file/x".to_owned()]),
        ["vanilla", "file/b"]
    );
}

#[tuff_core::watchdog]
#[test]
fn oversized_and_corrupt_pack_files_fail_the_load() {
    let (dir, backend) = world();
    let bomb = vec![b'#'; MAX_FILE + 1];
    let zip = zip::write(&[
        ("pack.mcmeta", meta("bomb").as_bytes()),
        ("data/a/function/big.mcfunction", &bomb),
    ]);
    assert!(zip.len() < 1024 * 1024);
    write(dir.path(), "datapacks/bomb.zip", &zip);
    let packs = discover(&*backend);
    let selected = configure(&packs, &DataPackConfig::default());
    let err = load(
        &*backend,
        &roots(),
        &Base::default(),
        &packs,
        &selected,
        1,
        None,
    )
    .unwrap_err();
    assert_eq!(err.pack, "file/bomb.zip");
    assert!(err.error.to_string().contains("limit"), "{err}");

    write(dir.path(), "datapacks/bomb.zip", b"truncated");
    let err = load(
        &*backend,
        &roots(),
        &Base::default(),
        &packs,
        &selected,
        1,
        None,
    )
    .unwrap_err();
    assert_eq!(err.pack, "file/bomb.zip");

    let (dir, backend) = world();
    write(
        dir.path(),
        "datapacks/huge/pack.mcmeta",
        meta("huge").as_bytes(),
    );
    write(
        dir.path(),
        "datapacks/huge/data/a/function/big.mcfunction",
        &bomb,
    );
    let packs = discover(&*backend);
    let err = load(
        &*backend,
        &roots(),
        &Base::default(),
        &packs,
        &configure(&packs, &DataPackConfig::default()),
        1,
        None,
    )
    .unwrap_err();
    assert!(err.error.to_string().contains("limit"), "{err}");
}

#[tuff_core::watchdog]
#[test]
fn a_sqlite_world_keeps_its_packs_in_the_archive() {
    let dir = TestDir::new();
    let backend: Arc<dyn WorldBackend> =
        Arc::new(tuff_storage::sqlite::SqliteBackend::open(dir.path().join("w.mcserver")).unwrap());
    backend
        .write_extra_file("datapacks/s/pack.mcmeta", meta("in sqlite").as_bytes())
        .unwrap();
    backend
        .write_extra_file(
            "datapacks/s/data/a/function/f.mcfunction",
            b"say from sqlite",
        )
        .unwrap();
    let zip = zip::write(&[
        ("pack.mcmeta", meta("zip in sqlite").as_bytes()),
        ("data/a/function/z.mcfunction", b"say zipped"),
    ]);
    backend.write_extra_file("datapacks/z.zip", &zip).unwrap();
    backend.write_extra_file("other/file", b"x").unwrap();
    let packs = discover(&*backend);
    assert_eq!(ids(&packs), ["vanilla", "file/s", "file/z.zip"]);
    let selected = configure(&packs, &DataPackConfig::default());
    let contents = load(
        &*backend,
        &roots(),
        &Base::default(),
        &packs,
        &selected,
        1,
        None,
    )
    .unwrap();
    assert_eq!(
        contents.functions.keys().collect::<Vec<_>>(),
        ["a:f", "a:z"]
    );
    let err = backend
        .read_extra_file_capped("datapacks/s/data/a/function/f.mcfunction", 4)
        .unwrap_err();
    assert!(err.to_string().contains("limit"), "{err}");
}

#[tuff_core::watchdog]
#[test]
fn capped_reads_refuse_paths_outside_the_world() {
    let (dir, backend) = world();
    write(dir.path(), "datapacks/x/pack.mcmeta", b"{}");
    assert!(backend.read_extra_file_capped("../outside", 100).is_err());
    assert!(backend.read_extra_file_capped("/etc/hosts", 100).is_err());
    assert!(backend.list_extra_files_under("../").is_err());
    assert_eq!(
        backend
            .read_extra_file_capped("datapacks/x/pack.mcmeta", 2)
            .unwrap(),
        b"{}"
    );
    assert!(
        backend
            .read_extra_file_capped("datapacks/x/pack.mcmeta", 1)
            .is_err()
    );
}

#[tuff_core::watchdog]
#[test]
fn identifiers_are_validated() {
    assert_eq!(identifier("foo").as_deref(), Some("minecraft:foo"));
    assert_eq!(identifier("a:b/c.d").as_deref(), Some("a:b/c.d"));
    assert_eq!(identifier("A:b"), None);
    assert_eq!(identifier("a:b c"), None);
    assert_eq!(quote_if_needed("file/x"), "\"file/x\"");
    assert_eq!(quote_if_needed("vanilla"), "vanilla");
}

struct ShapelessOnly;

impl DataRecipes for ShapelessOnly {
    fn recipe(
        &self,
        entry: &tuff_vanilla_data::recipe::RecipeEntry,
    ) -> Arc<dyn crate::world::crafting::Recipe> {
        let tuff_vanilla_data::recipe::Recipe::CraftingShapeless(recipe) = &entry.recipe else {
            panic!("{} is not shapeless", entry.name);
        };
        Arc::new(crate::world::crafting::test_recipe::Shapeless {
            recipe: recipe.clone(),
        })
    }
}

fn shapeless_base() -> Base {
    Base {
        data_recipes: Some(Arc::new(ShapelessOnly)),
        ..Base::default()
    }
}

fn write_pack_files(root: &Path, files: &[(&str, String)]) {
    for (path, contents) in files {
        write(
            root,
            &format!("datapacks/p/data/{path}"),
            contents.as_bytes(),
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn pack_structure_templates_load_by_id() {
    let (dir, backend) = world();
    write(dir.path(), "datapacks/p/pack.mcmeta", meta("p").as_bytes());
    let files = [("a/structure/huts/small.nbt", "template-bytes".to_owned())];
    write_pack_files(dir.path(), &files);
    let packs = discover(&*backend);
    let selected = configure(&packs, &DataPackConfig::default());
    let contents = load(
        &*backend,
        &roots(),
        &shapeless_base(),
        &packs,
        &selected,
        1,
        None,
    )
    .unwrap();
    assert_eq!(
        contents.structures.get("a:huts/small").map(|b| &b[..]),
        Some(&b"template-bytes"[..])
    );
}

#[tuff_core::watchdog]
#[test]
fn pack_tags_recipes_and_loot_tables_load() {
    let (dir, backend) = world();
    write(dir.path(), "datapacks/p/pack.mcmeta", meta("p").as_bytes());
    let files = [
        (
            "a/tags/item/gems.json",
            r#"{"values":["minecraft:diamond"]}"#,
        ),
        (
            "minecraft/tags/item/logs.json",
            r##"{"values":["#a:gems",{"id":"a:missing","required":false}]}"##,
        ),
        (
            "a/predicate/never.json",
            r#"{"type":"random_chance","chance":0}"#,
        ),
        (
            "minecraft/loot_table/blocks/stone.json",
            r##"{"pools":[{"rolls":1,"condition":"a:never","entries":[{"type":"tag","name":"a:gems"}]}]}"##,
        ),
        ("minecraft/loot_table/blocks/dirt.json", "{broken"),
        (
            "a/item_modifier/three.json",
            r#"[{"type":"set_count","count":3,"condition":"a:never"}]"#,
        ),
        ("a/item_modifier/broken.json", r#"{"type":"nope"}"#),
        (
            "a/slot_source/hands.json",
            r#"[{"type":"slot_range","source":"this","slots":"weapon.mainhand"},"a:hands"]"#,
        ),
        (
            "a/slot_source/broken.json",
            r#"{"type":"slot_range","slots":"nope"}"#,
        ),
        (
            "a/recipe/r.json",
            r##"{"type":"crafting_shapeless","ingredients":["#a:gems"],"result":"dirt"}"##,
        ),
    ];
    let files = files.map(|(path, contents)| (path, contents.to_owned()));
    write_pack_files(dir.path(), &files);
    let packs = discover(&*backend);
    let selected = configure(&packs, &DataPackConfig::default());
    let contents = load(
        &*backend,
        &roots(),
        &shapeless_base(),
        &packs,
        &selected,
        1,
        None,
    )
    .unwrap();

    let recipes = contents.recipes.as_ref().unwrap();
    let grid = [Some(tuff_item::item::ItemStack::new(
        tuff_vanilla_data::Item::Diamond,
        1,
    ))];
    assert_eq!(
        recipes.grid_result(&grid, 1, 1),
        Some(tuff_item::item::ItemStack::new(
            tuff_vanilla_data::Item::Dirt,
            1
        ))
    );
    let logs = contents
        .tags
        .typed::<tuff_vanilla_data::Item>("minecraft:logs")
        .unwrap();
    assert!(logs.contains(&tuff_vanilla_data::Item::Diamond));
    assert!(logs.contains(&tuff_vanilla_data::Item::OakLog));
    assert!(contents.tags.get("minecraft:item", "a:gems").is_some());
    let stone = contents.loot_tables.get("minecraft:blocks/stone").unwrap();
    assert_eq!(
        stone.pools[0].condition.as_ref().unwrap().kind,
        "minecraft:random_chance"
    );
    assert!(contents.loot_tables.get("minecraft:blocks/dirt").is_none());
    assert!(
        contents
            .loot_tables
            .get("minecraft:blocks/oak_log")
            .is_some()
    );
    assert!(contents.block_loot.is_none(), "no feature rolls loot here");
    let three = contents.item_modifiers.get("a:three").unwrap();
    assert_eq!(&*three[0].kind, "minecraft:set_count");
    assert!(contents.item_modifiers.get("a:broken").is_none());
    assert!(contents.slot_sources.get("a:hands").is_some());
    assert!(contents.slot_sources.get("a:broken").is_none());
}

/// Weak handles to what one load's pack data owns.
struct Owned {
    recipes: std::sync::Weak<RecipeSet>,
    name: std::sync::Weak<str>,
    ingredient: std::sync::Weak<[tuff_vanilla_data::Item]>,
    tables: std::sync::Weak<loot::LootTables>,
    pools: std::sync::Weak<[tuff_vanilla_data::loot_table::LootPool]>,
}

impl Owned {
    fn of(contents: &Contents, recipe: &str) -> Self {
        let recipes = contents.recipes.as_ref().unwrap();
        let (name, recipe) = recipes
            .entries()
            .find(|(name, _)| **name == recipe)
            .unwrap();
        let tuff_vanilla_data::Frozen::Shared(name) = name else {
            panic!("{name} is compiled in");
        };
        let placement = recipe.placement().unwrap();
        let tuff_vanilla_data::recipe::Ingredient::AnyOf(tuff_vanilla_data::Frozen::Shared(items)) =
            &placement.ingredients[0]
        else {
            panic!("pack ingredients are runtime data");
        };
        let stone = contents.loot_tables.get("minecraft:blocks/stone").unwrap();
        let tuff_vanilla_data::Frozen::Shared(pools) = &stone.pools else {
            panic!("pack loot pools are runtime data");
        };
        Self {
            recipes: Arc::downgrade(recipes),
            name: Arc::downgrade(name),
            ingredient: Arc::downgrade(items),
            tables: Arc::downgrade(&contents.loot_tables),
            pools: Arc::downgrade(pools),
        }
    }

    fn freed(&self) -> bool {
        self.recipes.strong_count() == 0
            && self.name.strong_count() == 0
            && self.ingredient.strong_count() == 0
            && self.tables.strong_count() == 0
            && self.pools.strong_count() == 0
    }
}

#[tuff_core::watchdog]
#[test]
fn reloading_changed_packs_frees_the_previous_pack_data() {
    let (dir, backend) = world();
    write(dir.path(), "datapacks/p/pack.mcmeta", meta("p").as_bytes());
    let base = shapeless_base();
    let mut previous: Option<(Contents, Owned)> = None;
    for generation in 1..=20u64 {
        let recipe = format!("a:r{generation}");
        let old = format!("a/recipe/r{}.json", generation - 1);
        let _ = std::fs::remove_file(dir.path().join("datapacks/p/data").join(&old));
        let items: Vec<String> = (0..generation)
            .map(|i| {
                format!(
                    "\"{}\"",
                    tuff_vanilla_data::Item::ALL[1 + i as usize].name()
                )
            })
            .collect();
        let items = items.join(",");
        write_pack_files(
            dir.path(),
            &[
                (
                    &format!("a/recipe/r{generation}.json"),
                    format!(
                        r#"{{"type":"crafting_shapeless","group":"g{generation}","ingredients":[[{items}]],"result":"dirt"}}"#
                    ),
                ),
                (
                    "minecraft/loot_table/blocks/stone.json",
                    format!(
                        r#"{{"pools":[{{"rolls":{generation},"entries":[{{"type":"item","name":"diamond"}}]}}]}}"#
                    ),
                ),
            ],
        );
        let packs = discover(&*backend);
        let selected = configure(&packs, &DataPackConfig::default());
        let contents = load(
            &*backend,
            &roots(),
            &base,
            &packs,
            &selected,
            generation,
            None,
        )
        .unwrap();
        let recipes = contents.recipes.as_ref().unwrap();
        assert!(recipes.get(&recipe).is_some());
        assert!(recipes.get(&format!("a:r{}", generation - 1)).is_none());
        let owned = Owned::of(&contents, &recipe);
        if let Some((old_contents, old_owned)) = previous.take() {
            assert!(!old_owned.freed(), "still in use until replaced");
            drop(old_contents);
            assert!(
                old_owned.freed(),
                "generation {} kept pack data alive",
                generation - 1
            );
        }
        previous = Some((contents, owned));
    }
}

#[tuff_core::watchdog]
#[test]
fn pack_tags_reach_built_in_tag_ingredients() {
    let (dir, backend) = world();
    write(dir.path(), "datapacks/p/pack.mcmeta", meta("p").as_bytes());
    write_pack_files(
        dir.path(),
        &[(
            "minecraft/tags/item/oak_logs.json",
            r#"{"values":["minecraft:stone"]}"#.to_owned(),
        )],
    );
    let planks = tuff_vanilla_data::recipe::RecipeEntry::get("minecraft:oak_planks").unwrap();
    let mut base = shapeless_base();
    base.recipes = Arc::new(RecipeSet::new([(
        planks.name.clone(),
        ShapelessOnly.recipe(planks),
    )]));
    let packs = discover(&*backend);
    let selected = configure(&packs, &DataPackConfig::default());
    let contents = load(&*backend, &roots(), &base, &packs, &selected, 1, None).unwrap();
    let recipes = contents.recipes.as_ref().unwrap();
    let crafted =
        |item| recipes.grid_result(&[Some(tuff_item::item::ItemStack::new(item, 1))], 1, 1);
    let four_planks = Some(tuff_item::item::ItemStack::new(
        tuff_vanilla_data::Item::OakPlanks,
        4,
    ));
    assert_eq!(crafted(tuff_vanilla_data::Item::Stone), four_planks);
    assert_eq!(crafted(tuff_vanilla_data::Item::OakLog), four_planks);
    assert_eq!(
        base.recipes.grid_result(
            &[Some(tuff_item::item::ItemStack::new(
                tuff_vanilla_data::Item::Stone,
                1
            ))],
            1,
            1
        ),
        None
    );
}

#[tuff_core::watchdog]
#[test]
fn feature_packs_are_offered_but_never_enabled_on_their_own() {
    let (dir, backend) = world();
    write(dir.path(), "datapacks/p/pack.mcmeta", meta("p").as_bytes());
    let packs = discover(&*backend);
    let features: Vec<&PackInfo> = packs
        .iter()
        .filter(|p| p.source == PackSource::Feature)
        .collect();
    assert_eq!(
        features.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        [
            "minecart_improvements",
            "redstone_experiments",
            "trade_rebalance"
        ]
    );
    let trades = features[2];
    assert_eq!(trades.meta.features, ["minecraft:trade_rebalance"]);
    assert_eq!(trades.missing_features(), ["minecraft:trade_rebalance"]);
    assert_eq!(
        trades.chat_link(false).plain(),
        "[trade_rebalance (feature)]"
    );
    assert_eq!(
        configure(&packs, &DataPackConfig::default()),
        ["vanilla", "file/p"]
    );
    let saved = DataPackConfig {
        enabled: vec!["vanilla".into(), "trade_rebalance".into()],
        disabled: Vec::new(),
    };
    let selected = configure(&packs, &saved);
    assert_eq!(selected, ["vanilla", "file/p"]);
    let config = saved_config(&packs, &selected, &saved);
    assert!(config.disabled.contains(&"trade_rebalance".to_owned()));
}

#[tuff_core::watchdog]
#[test]
fn a_new_world_takes_the_flags_of_the_feature_packs_it_starts_with() {
    let (_dir, backend) = world();
    let mut packs = discover(&*backend);
    let start = DataPackConfig {
        enabled: vec!["vanilla".into(), "trade_rebalance".into()],
        disabled: Vec::new(),
    };
    let (selected, flags) = configure_world(&mut packs, &start, None);
    assert_eq!(selected, ["vanilla", "trade_rebalance"]);
    assert_eq!(&*flags, ["minecraft:trade_rebalance", "minecraft:vanilla"]);
    let pack = packs.iter().find(|p| p.id == "trade_rebalance").unwrap();
    assert!(pack.missing_features().is_empty());
    let redstone = packs
        .iter()
        .find(|p| p.id == "redstone_experiments")
        .unwrap();
    assert_eq!(
        redstone.missing_features(),
        ["minecraft:redstone_experiments"],
        "an open world's flags are fixed"
    );

    let (selected, flags) = configure_world(&mut packs, &DataPackConfig::default(), None);
    assert_eq!(selected, ["vanilla"]);
    assert_eq!(&*flags, ["minecraft:vanilla"]);
}

#[tuff_core::watchdog]
#[test]
fn a_saved_flag_forces_its_feature_pack_and_a_missing_one_drops_it() {
    let (_dir, backend) = world();
    let mut packs = discover(&*backend);
    let flags = [
        "minecraft:vanilla".to_owned(),
        "minecraft:trade_rebalance".to_owned(),
    ];
    let (selected, kept) = configure_world(&mut packs, &DataPackConfig::default(), Some(&flags));
    assert_eq!(selected, ["vanilla", "trade_rebalance"]);
    assert_eq!(&*kept, ["minecraft:trade_rebalance", "minecraft:vanilla"]);

    let saved = DataPackConfig {
        enabled: vec!["vanilla".into(), "trade_rebalance".into()],
        disabled: Vec::new(),
    };
    let vanilla = ["minecraft:vanilla".to_owned()];
    let (selected, kept) = configure_world(&mut packs, &saved, Some(&vanilla));
    assert_eq!(selected, ["vanilla"]);
    assert_eq!(&*kept, ["minecraft:vanilla"]);
}

#[tuff_core::watchdog]
#[test]
fn an_enabled_feature_pack_loads_its_compiled_in_files() {
    let (_dir, backend) = world();
    let mut packs = discover(&*backend);
    let start = DataPackConfig {
        enabled: vec!["vanilla".into(), "trade_rebalance".into()],
        disabled: Vec::new(),
    };
    let (selected, _) = configure_world(&mut packs, &start, None);
    let contents = load(
        &*backend,
        &roots(),
        &Base::default(),
        &packs,
        &selected,
        1,
        None,
    )
    .unwrap();
    let librarian = contents
        .tags
        .get("minecraft:villager_trade", "minecraft:librarian/level_1")
        .unwrap();
    assert!(
        librarian
            .iter()
            .any(|t| t == "minecraft:librarian/1/emerald_and_book_desert_enchanted_book"),
        "{librarian:?}"
    );
    let plain = load(
        &*backend,
        &roots(),
        &Base::default(),
        &packs,
        &["vanilla".into()],
        1,
        None,
    )
    .unwrap();
    let built_in = plain
        .tags
        .get("minecraft:villager_trade", "minecraft:librarian/level_1")
        .unwrap();
    assert!(!built_in.iter().any(|t| t.contains("desert_enchanted_book")));
    assert!(plain.loot_tables.is_vanilla());
    assert!(
        contents
            .loot_tables
            .get("minecraft:chests/abandoned_mineshaft")
            .is_some()
    );
    assert!(
        !contents.loot_tables.is_vanilla(),
        "the pack's chest loot replaces the built-in tables"
    );
}

#[tuff_core::watchdog]
#[test]
fn a_pack_filter_hides_lower_packs_and_built_in_content() {
    let (dir, backend) = world();
    write(
        dir.path(),
        "datapacks/a_low/pack.mcmeta",
        meta("low").as_bytes(),
    );
    for (path, contents) in [
        ("a/function/gone.mcfunction", "say gone"),
        ("a/function/kept.mcfunction", "say kept"),
        ("b/function/gone.mcfunction", "say other namespace"),
    ] {
        write(
            dir.path(),
            &format!("datapacks/a_low/data/{path}"),
            contents.as_bytes(),
        );
    }
    let current = meta::Format::current().major;
    let filtering = format!(
        r#"{{"pack":{{"description":"f","min_format":{current},"max_format":{current}}},
            "filter":{{"block":[{{"namespace":"^(a|minecraft)$","path":"gone|^enchantment/lunge|^recipe/oak_planks|blocks/dirt|tags/item/planks|story/mine_stone"}}]}}}}"#
    );
    write(
        dir.path(),
        "datapacks/b_filter/pack.mcmeta",
        filtering.as_bytes(),
    );
    write(
        dir.path(),
        "datapacks/b_filter/data/a/function/gone.mcfunction",
        b"say own file stays",
    );
    write(
        dir.path(),
        "datapacks/c_high/pack.mcmeta",
        meta("high").as_bytes(),
    );
    write(
        dir.path(),
        "datapacks/c_high/data/minecraft/tags/item/planks.json",
        br#"{"values":["minecraft:stone"]}"#,
    );
    let planks = tuff_vanilla_data::recipe::RecipeEntry::get("minecraft:oak_planks").unwrap();
    let mut base = shapeless_base();
    base.recipes = Arc::new(RecipeSet::new([(
        planks.name.clone(),
        ShapelessOnly.recipe(planks),
    )]));
    let packs = discover(&*backend);
    let selected = configure(&packs, &DataPackConfig::default());
    assert_eq!(
        selected,
        ["vanilla", "file/a_low", "file/b_filter", "file/c_high"]
    );
    let contents = load(&*backend, &roots(), &base, &packs, &selected, 1, None).unwrap();

    assert_eq!(
        contents.functions.keys().collect::<Vec<_>>(),
        ["a:gone", "a:kept", "b:gone"]
    );
    let gone = &contents.functions["a:gone"];
    let call = gone.prepare(None, &roots()).unwrap();
    let lines: Vec<&str> = gone.commands(&call).map(|(input, _)| input).collect();
    assert_eq!(lines, ["say own file stays"]);

    assert!(
        contents
            .recipes
            .as_ref()
            .unwrap()
            .get("minecraft:oak_planks")
            .is_none()
    );
    assert!(base.recipes.get("minecraft:oak_planks").is_some());
    let enchantments = contents.registries.get("minecraft:enchantment").unwrap();
    assert!(!enchantments.contains("minecraft:lunge"));
    assert!(enchantments.contains("minecraft:sharpness"));
    assert!(enchantments.modified());
    assert!(contents.loot_tables.get("minecraft:blocks/dirt").is_none());
    assert!(contents.loot_tables.get("minecraft:blocks/stone").is_some());
    let planks = contents
        .tags
        .typed::<tuff_vanilla_data::Item>("minecraft:planks")
        .unwrap();
    assert_eq!(planks, [tuff_vanilla_data::Item::Stone]);
    assert!(contents.advancements.is_some());
}

#[tuff_core::watchdog]
#[test]
fn startup_feature_packs_toggle_a_saved_worlds_flags_each_time() {
    let (_dir, backend) = world();
    let mut packs = discover(&*backend);
    let saved = DataPackConfig {
        enabled: vec!["vanilla".into()],
        disabled: Vec::new(),
    };
    let vanilla = vec!["minecraft:vanilla".to_owned()];
    let on = ["minecart_improvements".to_owned()];

    let (config, flags) = apply_startup_feature_packs(saved, Some(vanilla.clone()), &on);
    let (selected, kept) = configure_world(&mut packs, &config, flags.as_deref());
    assert!(selected.contains(&"minecart_improvements".to_owned()));
    assert!(kept.contains(&"minecraft:minecart_improvements".to_owned()));

    let (config, flags) = apply_startup_feature_packs(config, flags, &[]);
    let (selected, kept) = configure_world(&mut packs, &config, flags.as_deref());
    assert_eq!(selected, ["vanilla"]);
    assert_eq!(&*kept, ["minecraft:vanilla"]);

    let (config, flags) = apply_startup_feature_packs(config, None, &on);
    assert_eq!(flags, None, "a new world keeps its own starting packs");
    assert!(!config.enabled.contains(&"minecart_improvements".to_owned()));
}

#[tuff_core::watchdog]
#[test]
fn vanilla_network_body_is_the_configuration_one() {
    assert_eq!(
        tags::TagSet::vanilla().network_body(),
        crate::protocol::configuration::update_tags().0
    );
}

#[tuff_core::watchdog]
#[test]
fn a_feed_follows_the_newest_load_else_the_scope() {
    use tags::{TagFeed, sync_scope};
    use tuff_vanilla_data::registry::Block;
    use tuff_vanilla_data::tag::block::key::LOGS;
    let pack = tags::vanilla_with(&[("minecraft:block", "minecraft:logs", "minecraft:stone")]);
    assert!(sync_scope(pack.clone(), || TagFeed::default()
        .current()
        .is(LOGS, Block::Stone)));
    assert!(!TagFeed::default().current().is(LOGS, Block::Stone));
    let (contents, rx) = tokio::sync::watch::channel(Arc::new(Contents::default()));
    let feed = TagFeed::new(rx);
    let before = feed.current();
    contents
        .send(Arc::new(Contents {
            tags: pack.clone(),
            ..Default::default()
        }))
        .unwrap();
    let after = feed.current();
    assert!(!before.is(LOGS, Block::Stone));
    assert!(Arc::ptr_eq(&after, &pack));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_panic_restarts_the_state_actor() {
    let (_dir, backend) = world();
    let packs = DataPacks::open_now(
        backend,
        Arc::default(),
        Base::default(),
        DataPackConfig::default(),
        None,
    );
    let before = packs.status(false).await.unwrap();
    packs.panic_for_test().await;
    assert_eq!(packs.status(false).await.unwrap().selected, before.selected);
}

#[tuff_core::watchdog]
#[test]
fn registry_entries_load_at_startup_and_a_bad_one_drops_its_pack() {
    let (dir, backend) = world();
    for name in ["good", "bad"] {
        write(
            dir.path(),
            &format!("datapacks/{name}/pack.mcmeta"),
            meta(name).as_bytes(),
        );
    }
    write(
        dir.path(),
        "datapacks/good/data/a/damage_type/zap.json",
        br#"{"message_id":"zap","scaling":"never","exhaustion":0.0}"#,
    );
    write(
        dir.path(),
        "datapacks/good/data/a/tags/damage_type/shocks.json",
        br#"{"values":["a:zap"]}"#,
    );
    write(
        dir.path(),
        "datapacks/bad/data/b/damage_type/broken.json",
        b"{oops",
    );
    let opened = initial_load(
        &*backend,
        &roots(),
        &Base::default(),
        &DataPackConfig::default(),
        None,
    );
    assert!(opened.selected.contains(&"file/good".to_owned()));
    assert!(!opened.selected.contains(&"file/bad".to_owned()));
    let registries = &opened.contents.registries;
    let damage = registries.get("minecraft:damage_type").unwrap();
    assert!(damage.contains("a:zap"));
    assert!(!damage.contains("b:broken"));
    assert!(
        !registries
            .get("minecraft:worldgen/biome")
            .unwrap()
            .modified()
    );
    assert_eq!(
        opened
            .contents
            .tags
            .get("minecraft:damage_type", "a:shocks"),
        Some(&["a:zap".to_owned()][..])
    );
    let zap = damage.index_of("a:zap").unwrap() as i32;
    let mut r = tuff_wire::PacketReader::new(opened.contents.tags_packet.clone());
    let registries_tagged = r.read_var_int().unwrap();
    let mut found = false;
    for _ in 0..registries_tagged {
        let key = r.read_string().unwrap();
        for _ in 0..r.read_var_int().unwrap() {
            let name = r.read_string().unwrap();
            let ids: Vec<i32> = (0..r.read_var_int().unwrap())
                .map(|_| r.read_var_int().unwrap())
                .collect();
            if key == "minecraft:damage_type" && name == "a:shocks" {
                assert_eq!(ids, [zap]);
                found = true;
            }
        }
    }
    assert!(found);

    write(
        dir.path(),
        "datapacks/good/data/a/damage_type/later.json",
        br#"{"message_id":"later","scaling":"never","exhaustion":0.0}"#,
    );
    let reloaded = load(
        &*backend,
        &roots(),
        &Base::default(),
        &opened.packs,
        &opened.selected,
        2,
        Some(registries.clone()),
    )
    .unwrap();
    assert!(Arc::ptr_eq(&reloaded.registries, registries));
}

#[tuff_core::watchdog]
#[test]
fn vanilla_only_registries_are_unmodified() {
    let (_dir, backend) = world();
    let opened = initial_load(
        &*backend,
        &roots(),
        &Base::default(),
        &DataPackConfig::default(),
        None,
    );
    assert!(!opened.contents.registries.modified());
    assert!(Arc::ptr_eq(
        &opened.contents.registries,
        &tuff_item::registries::Registries::vanilla()
    ));
}

#[tuff_core::watchdog]
#[test]
fn a_pack_overrides_an_enchantment_biome_and_dimension_type_and_adds_a_trade() {
    let (dir, backend) = world();
    write(
        dir.path(),
        "datapacks/fix/pack.mcmeta",
        meta("fix").as_bytes(),
    );
    let files: [(&str, &[u8]); 6] = [
        (
            "minecraft/enchantment/sharpness.json",
            br##"{"anvil_cost":1,"description":{"translate":"enchantment.minecraft.sharpness"},"effects":{"minecraft:damage":[{"effect":{"type":"minecraft:add","value":{"type":"minecraft:linear","base":2.0,"per_level_above_first":1.0}}}]},"exclusive_set":"#minecraft:exclusive_set/damage","max_cost":{"base":21,"per_level_above_first":11},"max_level":7,"min_cost":{"base":1,"per_level_above_first":11},"primary_items":"#minecraft:enchantable/melee_weapon","slots":["mainhand"],"supported_items":"#minecraft:enchantable/sharp_weapon","weight":10}"##,
        ),
        (
            "fix/villager_trade/bread.json",
            br#"{"wants": {"id": "minecraft:emerald", "count": 3}, "gives": {"id": "minecraft:bread", "count": 2}, "max_uses": 7, "xp": 9}"#,
        ),
        (
            "fix/tags/villager_trade/breads.json",
            br#"{"values": ["fix:bread"]}"#,
        ),
        (
            "minecraft/worldgen/biome/the_void.json",
            br##"{"attributes":{"minecraft:visual/sky_color":"#7ba4ff"},"carvers":[],"downfall":0.5,"effects":{"water_color":"#3f76e4"},"features":[],"has_precipitation":false,"temperature":0.5}"##,
        ),
        (
            "minecraft/dimension_type/overworld_caves.json",
            br##"{"ambient_light":0.25,"coordinate_scale":1.0,"has_ceiling":false,"has_ender_dragon_fight":false,"has_skylight":true,"height":512,"infiniburn":"#minecraft:infiniburn_overworld","logical_height":512,"min_y":-128,"monster_spawn_block_light_limit":0,"monster_spawn_light_level":7}"##,
        ),
        (
            "fix/dimension/tall.json",
            br#"{"type":"minecraft:overworld_caves","generator":{"type":"minecraft:flat","settings":{"biome":"minecraft:the_void","layers":[{"block":"minecraft:stone","height":1}]}}}"#,
        ),
    ];
    for (path, bytes) in files {
        write(dir.path(), &format!("datapacks/fix/data/{path}"), bytes);
    }
    let opened = initial_load(
        &*backend,
        &roots(),
        &Base::default(),
        &DataPackConfig::default(),
        None,
    );
    assert!(opened.selected.contains(&"file/fix".to_owned()));
    let contents = &opened.contents;
    let sharpness = contents.enchantments.get("minecraft:sharpness").unwrap();
    assert_eq!(sharpness.max_level, 7);
    assert!(
        !contents
            .enchantments
            .entry("minecraft:sharpness")
            .unwrap()
            .is_builtin()
    );
    assert!(contents.registries.trades().trades.contains("fix:bread"));
    assert_eq!(
        contents.tags.get("minecraft:villager_trade", "fix:breads"),
        Some(&["fix:bread".to_owned()][..])
    );
    for (key, id) in [
        ("minecraft:worldgen/biome", "minecraft:the_void"),
        ("minecraft:dimension_type", "minecraft:overworld_caves"),
        ("minecraft:dimension", "fix:tall"),
    ] {
        let registry = contents.registries.get(key).unwrap();
        assert!(!registry.entry(id).unwrap().is_builtin(), "{key} {id}");
    }
    let biome = contents
        .registries
        .get("minecraft:worldgen/biome")
        .unwrap()
        .get("minecraft:the_void")
        .unwrap();
    let nbt = biome.network.get().expect("biome network nbt");
    assert!(!nbt.windows(8).any(|w| w == b"features"));
    assert!(nbt.windows(11).any(|w| w == b"water_color"));
    let dimension = contents
        .registries
        .get("minecraft:dimension_type")
        .unwrap()
        .get("minecraft:overworld_caves")
        .unwrap();
    assert!(dimension.network.get().is_some());
}

#[tuff_core::watchdog]
#[test]
fn a_malformed_biome_or_dimension_type_drops_its_pack() {
    for (path, bytes) in [
        (
            "../minecraft/worldgen/biome/the_void.json",
            br#"{"carvers":[],"downfall":0.5,"effects":{},"features":[],"has_precipitation":false,"temperature":0.5}"#.as_slice(),
        ),
        (
            "../minecraft/dimension_type/overworld_caves.json",
            br##"{"ambient_light":0.0,"coordinate_scale":1.0,"has_ceiling":false,"has_ender_dragon_fight":false,"has_skylight":true,"height":100,"infiniburn":"#minecraft:infiniburn_overworld","logical_height":100,"min_y":0,"monster_spawn_block_light_limit":0,"monster_spawn_light_level":7}"##.as_slice(),
        ),
    ] {
        let (dir, backend) = world();
        write(dir.path(), "datapacks/bad/pack.mcmeta", meta("bad").as_bytes());
        write(dir.path(), &format!("datapacks/bad/data/b/{path}"), bytes);
        let opened = initial_load(
            &*backend,
            &roots(),
            &Base::default(),
            &DataPackConfig::default(),
            None,
        );
        assert!(!opened.selected.contains(&"file/bad".to_owned()), "{path}");
    }
}

#[tuff_core::watchdog]
#[test]
fn added_biomes_and_dimension_types_follow_the_built_in_ones() {
    let (dir, backend) = world();
    write(
        dir.path(),
        "datapacks/add/pack.mcmeta",
        meta("add").as_bytes(),
    );
    write(
        dir.path(),
        "datapacks/add/data/a/worldgen/biome/glade.json",
        br##"{"carvers":[],"downfall":0.5,"effects":{"water_color":"#3f76e4"},"features":[],"has_precipitation":false,"temperature":0.5}"##,
    );
    write(
        dir.path(),
        "datapacks/add/data/a/dimension_type/aaa.json",
        br##"{"ambient_light":0.5,"coordinate_scale":1.0,"has_ceiling":false,"has_ender_dragon_fight":false,"has_skylight":true,"height":128,"infiniburn":"#minecraft:infiniburn_overworld","logical_height":128,"min_y":0,"monster_spawn_block_light_limit":0,"monster_spawn_light_level":7}"##,
    );
    let opened = initial_load(
        &*backend,
        &roots(),
        &Base::default(),
        &DataPackConfig::default(),
        None,
    );
    assert!(opened.selected.contains(&"file/add".to_owned()));
    let registries = &opened.contents.registries;
    for (registry, id) in [
        (&tuff_vanilla_data::data_registry::WORLDGEN_BIOME, "a:glade"),
        (&tuff_vanilla_data::data_registry::DIMENSION_TYPE, "a:aaa"),
    ] {
        let frozen = registries.get(registry.key).unwrap();
        for (index, builtin) in registry.entries.iter().enumerate() {
            assert_eq!(frozen.index_of(builtin), Some(index as u32), "{builtin}");
        }
        assert_eq!(frozen.index_of(id), Some(registry.entries.len() as u32));
        assert!(frozen.get(id).unwrap().network.get().is_some());
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_pack_added_damage_type_keeps_built_in_damage_event_ids() {
    let (dir, backend) = world();
    write(
        dir.path(),
        "datapacks/fix/pack.mcmeta",
        meta("fix").as_bytes(),
    );
    write(
        dir.path(),
        "datapacks/fix/data/a/damage_type/aaa.json",
        br#"{"message_id":"aaa","scaling":"never","exhaustion":0.0}"#,
    );
    let opened = initial_load(
        &*backend,
        &roots(),
        &Base::default(),
        &DataPackConfig::default(),
        None,
    );
    let registries = opened.contents.registries.clone();
    let trident = &tuff_vanilla_data::damage_type::TRIDENT;
    let vanilla = crate::world::damage_type_id(trident);
    let expected = registries
        .get("minecraft:damage_type")
        .unwrap()
        .index_of("minecraft:trident")
        .unwrap() as i32;
    assert_eq!(expected, vanilla);
    let added = registries
        .get("minecraft:damage_type")
        .unwrap()
        .index_of("a:aaa")
        .unwrap() as usize;
    assert_eq!(
        added,
        tuff_vanilla_data::data_registry::DAMAGE_TYPE.entries.len()
    );
    let scoped =
        tuff_item::registries::scope(registries, async { crate::world::damage_type_id(trident) })
            .await;
    assert_eq!(scoped, expected);
}
