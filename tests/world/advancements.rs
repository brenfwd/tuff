//! Advancements on a real session: criteria met by inventory changes and
//! other triggers, the Update Advancements packets, saving and loading,
//! `/advancement`, the chat announcement and the recipe book.

use super::*;
use crate::protocol::play::Serverbound;
use crate::world::inventory::Inventory;
use crate::world::messages::{Directive, DirectiveSource};
use crate::world::predicate::{DamageFacts, EntityFacts, Flags, LocationFacts};
use crate::world::session::PlayerSession;
use crate::world::session::hooks::PlayerSaved;
use tuff_core::world::advancement::{SavedProgress, SessionAdvancements};
use tuff_wire::PacketReader;

type Progress = Vec<(String, Vec<(String, Option<i64>)>)>;

#[derive(Debug, Default)]
struct Added {
    id: String,
    parent: Option<String>,
    /// `(title, frame, flags, background)`.
    display: Option<(Tag, i32, i32, Option<String>)>,
    requirements: Vec<Vec<String>>,
    position: (f32, f32),
}

#[derive(Debug, Default)]
struct UpdatePacket {
    reset: bool,
    added: Vec<Added>,
    removed: Vec<String>,
    progress: Progress,
    show: bool,
}

fn parse_update(mut r: PacketReader) -> UpdatePacket {
    let mut packet = UpdatePacket {
        reset: r.read_bool().unwrap(),
        ..UpdatePacket::default()
    };
    for _ in 0..r.read_var_int().unwrap() {
        let mut added = Added {
            id: r.read_string().unwrap(),
            ..Added::default()
        };
        if r.read_bool().unwrap() {
            added.parent = Some(r.read_string().unwrap());
        }
        if r.read_bool().unwrap() {
            let title = r.read_nbt().unwrap().unwrap();
            r.read_nbt().unwrap();
            r.read_var_int().unwrap();
            assert_eq!(r.read_var_int().unwrap(), 1, "icon count");
            assert_eq!(r.read_var_int().unwrap(), 0, "plain icons only here");
            assert_eq!(r.read_var_int().unwrap(), 0);
            let frame = r.read_var_int().unwrap();
            let flags = r.read_int().unwrap();
            let background = (flags & 1 != 0).then(|| r.read_string().unwrap());
            added.display = Some((title, frame, flags, background));
        }
        for _ in 0..r.read_var_int().unwrap() {
            let group = (0..r.read_var_int().unwrap())
                .map(|_| r.read_string().unwrap())
                .collect();
            added.requirements.push(group);
        }
        r.read_bool().unwrap();
        added.position = (r.read_float().unwrap(), r.read_float().unwrap());
        packet.added.push(added);
    }
    for _ in 0..r.read_var_int().unwrap() {
        packet.removed.push(r.read_string().unwrap());
    }
    for _ in 0..r.read_var_int().unwrap() {
        let id = r.read_string().unwrap();
        let criteria = (0..r.read_var_int().unwrap())
            .map(|_| {
                let name = r.read_string().unwrap();
                let time = r.read_bool().unwrap().then(|| r.read_long().unwrap());
                (name, time)
            })
            .collect();
        packet.progress.push((id, criteria));
    }
    packet.show = r.read_bool().unwrap();
    assert_eq!(r.remaining(), 0);
    packet
}

/// Every packet of one kind sent since the last call, as readers past the
/// id.
fn sent(outbound: &mut mpsc::UnboundedReceiver<bytes::Bytes>, id: i32) -> Vec<PacketReader> {
    let mut found = Vec::new();
    while let Ok(frame) = outbound.try_recv() {
        let mut r = framed_payload(&frame);
        if r.read_var_int().unwrap() == id {
            found.push(r);
        }
    }
    found
}

fn updates(outbound: &mut mpsc::UnboundedReceiver<bytes::Bytes>) -> Vec<UpdatePacket> {
    let id = tuff_vanilla_data::packet::play::Clientbound::UpdateAdvancements.id();
    sent(outbound, id).into_iter().map(parse_update).collect()
}

fn give(player: &mut PlayerSession, slot: usize, item: Item) {
    player
        .inventory_mut()
        .set_slot(slot, Some(ItemStack::new(item, 1)));
}

fn source() -> DirectiveSource {
    DirectiveSource {
        name: TextComponent::new("Server"),
        player: None,
    }
}

/// `/advancement` as the command hands it to a target: every criterion of
/// the named advancements, or one criterion of the only one.
async fn advancement(
    player: &mut PlayerSession,
    grant: bool,
    names: &[&str],
    criterion: Option<&str>,
) -> i32 {
    let tree = player.progress.advancement_tree.clone();
    let ids = names.iter().map(|name| tree.id(name).unwrap()).collect();
    let directive = Directive::Advancement {
        grant,
        tree,
        ids,
        criterion: criterion.map(str::to_owned),
        show: true,
    };
    player.apply_directive(directive, source()).await.value
}

async fn recipe(player: &mut PlayerSession, give: bool, recipe: &'static str) -> i32 {
    let directive = Directive::Recipes {
        give,
        recipes: Some(vec![recipe.into()]),
    };
    player.apply_directive(directive, source()).await.value
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_arriving_grants_its_advancement_and_the_client_is_told() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = session_with_outbound(&world, 1);
    player.stand_at([0.5, 100.0, 0.5], true);

    player.run_tick().await;
    let first = updates(&mut outbound);
    assert_eq!(first.len(), 1);
    assert!(first[0].reset, "the first update resets the client's tree");
    let added: Vec<&str> = first[0].added.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(
        added,
        [
            "minecraft:recipes/root",
            "minecraft:recipes/decorations/crafting_table"
        ],
        "the tick trigger unlocks the crafting table recipe"
    );
    assert!(player.knows_recipe("minecraft:crafting_table"));

    player.run_tick().await;
    assert!(updates(&mut outbound).is_empty(), "nothing changed");

    give(&mut player, 36, Item::CraftingTable);
    player.run_tick().await;
    assert!(player.advancement_done("minecraft:story/root"));
    let update = updates(&mut outbound).pop().expect("an update");
    assert!(!update.reset);
    assert!(update.show);
    let root = &update.added[0];
    assert_eq!(root.id, "minecraft:story/root");
    assert_eq!(root.parent, None);
    let (title, frame, flags, background) = root.display.as_ref().unwrap();
    assert_eq!(
        TextComponent::from_nbt(title).unwrap().translate.as_deref(),
        Some("advancements.story.root.title")
    );
    assert_eq!(*frame, 0);
    assert_eq!(*flags & 2, 0, "the root shows no toast");
    assert_eq!(
        background.as_deref(),
        Some("minecraft:gui/advancements/backgrounds/stone")
    );
    assert_eq!(root.requirements, [["crafting_table"]]);
    let stone = update
        .added
        .iter()
        .find(|a| a.id == "minecraft:story/mine_stone")
        .expect("children of a done advancement show");
    assert_eq!(stone.parent.as_deref(), Some("minecraft:story/root"));
    assert_eq!(stone.position.0, 1.0);
    let (_, criteria) = update
        .progress
        .iter()
        .find(|(id, _)| id == "minecraft:story/root")
        .unwrap();
    assert!(matches!(criteria.as_slice(), [(name, Some(_))] if name == "crafting_table"));
    let (_, stone_progress) = update
        .progress
        .iter()
        .find(|(id, _)| id == "minecraft:story/mine_stone")
        .unwrap();
    assert!(stone_progress.iter().all(|(_, time)| time.is_none()));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn progress_is_saved_as_the_game_writes_it_and_loaded_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 2);
    player.stand_at([0.5, 100.0, 0.5], true);
    give(&mut player, 36, Item::CraftingTable);
    player.run_tick().await;
    let granted = advancement(
        &mut player,
        true,
        &["minecraft:adventure/adventuring_time"],
        Some("minecraft:badlands"),
    )
    .await;
    assert_eq!(granted, 1);
    player.save().await;

    let file = dir.path().join("players/advancements").join(format!(
        "{}.json",
        tuff_storage::vanilla::player_file_stem(2)
    ));
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).expect("saved at the 26.x path")).unwrap();
    assert_eq!(
        json["minecraft:story/root"]["done"],
        serde_json::json!(true)
    );
    let date = json["minecraft:story/root"]["criteria"]["crafting_table"]
        .as_str()
        .unwrap();
    assert!(date.ends_with(" +0000") && date.len() == 25, "{date}");
    assert_eq!(
        json["minecraft:adventure/adventuring_time"]["done"],
        serde_json::json!(false)
    );
    assert!(json["DataVersion"].is_number());
    assert!(
        json.get("minecraft:story/mine_stone").is_none(),
        "no progress, not saved"
    );

    let (mut again, mut outbound) = session_with_outbound(&world, 2);
    again.load_saved().await;
    assert!(again.advancement_done("minecraft:story/root"));
    assert!(again.criterion_done("minecraft:adventure/adventuring_time", "minecraft:badlands"));
    assert!(!again.advancement_done("minecraft:adventure/adventuring_time"));
    again.stand_at([0.5, 100.0, 0.5], true);
    again.run_tick().await;
    let update = updates(&mut outbound).pop().unwrap();
    assert!(update.reset);
    assert!(update.added.iter().any(|a| a.id == "minecraft:story/root"));
    assert!(
        !update
            .added
            .iter()
            .any(|a| a.id == "minecraft:adventure/root"),
        "a started advancement shows nothing until one is done"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_advancement_command_grants_and_revokes_with_rewards_and_announcements() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = session_with_outbound(&world, 3);
    player.stand_at([0.5, 100.0, 0.5], true);
    player.run_tick().await;

    let until = ["minecraft:story/root", "minecraft:story/mine_stone"];
    assert_eq!(advancement(&mut player, true, &until, None).await, 2);
    player.run_tick().await;
    let shown = updates(&mut outbound).pop().unwrap();
    assert!(
        shown
            .added
            .iter()
            .any(|a| a.id == "minecraft:story/smelt_iron")
    );
    assert!(player.advancement_done("minecraft:story/root"));
    assert!(player.advancement_done("minecraft:story/mine_stone"));
    assert!(!player.advancement_done("minecraft:story/upgrade_tools"));
    let announced = world.announced().await;
    let stone = announced
        .iter()
        .find(|m| {
            m.with
                .get(1)
                .and_then(|name| name.with.first())
                .and_then(|title| title.translate.as_deref())
                == Some("advancements.story.mine_stone.title")
        })
        .expect("mine_stone is announced");
    assert_eq!(
        stone.translate.as_deref(),
        Some("chat.type.advancement.task")
    );
    assert_eq!(stone.with[0].text, "Player3");
    assert_eq!(stone.with[1].color.as_deref(), Some("green"));
    assert_eq!(
        stone.with[1].translate.as_deref(),
        Some("chat.square_brackets")
    );
    assert_eq!(announced.len(), 1, "the root does not announce");

    let before = player.experience().total;
    advancement(&mut player, true, &["minecraft:nether/fast_travel"], None).await;
    assert_eq!(player.experience().total, before + 100, "experience reward");
    let challenge = world.announced().await.last().cloned().unwrap();
    assert_eq!(
        challenge.translate.as_deref(),
        Some("chat.type.advancement.challenge")
    );

    let from = [
        "minecraft:story/mine_stone",
        "minecraft:story/upgrade_tools",
        "minecraft:story/smelt_iron",
    ];
    assert_eq!(advancement(&mut player, false, &from, None).await, 1);
    assert_eq!(advancement(&mut player, false, &from, None).await, 0);
    assert!(!player.advancement_done("minecraft:story/mine_stone"));
    assert!(player.advancement_done("minecraft:story/root"));
    player.run_tick().await;
    let update = updates(&mut outbound).pop().unwrap();
    assert!(
        update
            .removed
            .iter()
            .any(|id| id == "minecraft:story/smelt_iron")
    );
    assert!(
        !update
            .removed
            .iter()
            .any(|id| id == "minecraft:story/upgrade_tools")
    );

    let biomes = ["minecraft:adventure/adventuring_time"];
    let desert = Some("minecraft:desert");
    assert_eq!(advancement(&mut player, true, &biomes, desert).await, 1);
    assert_eq!(advancement(&mut player, true, &biomes, desert).await, 0);
    assert!(player.criterion_done(biomes[0], "minecraft:desert"));
    assert!(!player.advancement_done(biomes[0]), "one group of many");
    assert_eq!(advancement(&mut player, false, &biomes, desert).await, 1);
    assert!(!player.criterion_done(biomes[0], "minecraft:desert"));

    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "show_advancement_messages",
                crate::world::game_rules::RuleValue::Bool(false),
            )
            .expect("a boolean rule");
    });
    let heard = world.announced().await.len();
    advancement(&mut player, true, &["minecraft:story/smelt_iron"], None).await;
    assert!(player.advancement_done("minecraft:story/smelt_iron"));
    assert_eq!(world.announced().await.len(), heard, "rule off");

    while outbound.try_recv().is_ok() {}
    let everything = Directive::Advancement {
        grant: true,
        tree: player.progress.advancement_tree.clone(),
        ids: (0..player.progress.advancement_tree.len() as u16).collect(),
        criterion: None,
        show: false,
    };
    player.apply_directive(everything, source()).await;
    assert!(player.advancement_done("minecraft:end/elytra"));
    let id = tuff_vanilla_data::packet::play::Clientbound::UpdateAdvancements.id();
    let mut last = sent(&mut outbound, id).pop().unwrap();
    let bytes = last.read_bytes(last.remaining()).unwrap();
    assert_eq!(
        bytes.last(),
        Some(&0),
        "granting everything shows no toasts"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recipes_unlock_through_their_advancements_and_stay_taken() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = session_with_outbound(&world, 4);
    player.stand_at([0.5, 100.0, 0.5], true);
    player.run_tick().await;
    let add = tuff_vanilla_data::packet::play::Clientbound::RecipeBookAdd.id();

    give(&mut player, 36, Item::Redstone);
    player.run_tick().await;
    assert!(player.knows_recipe("minecraft:piston"));
    assert!(player.advancement_done("minecraft:recipes/redstone/piston"));
    assert!(!sent(&mut outbound, add).is_empty());

    assert!(recipe(&mut player, false, "minecraft:piston").await > 0);
    assert!(!player.knows_recipe("minecraft:piston"));
    give(&mut player, 37, Item::Redstone);
    player.run_tick().await;
    assert!(
        !player.knows_recipe("minecraft:piston"),
        "a done recipe advancement does not unlock again"
    );

    advancement(
        &mut player,
        false,
        &["minecraft:recipes/redstone/piston"],
        None,
    )
    .await;
    assert!(!player.advancement_done("minecraft:recipes/redstone/piston"));
    give(&mut player, 38, Item::Redstone);
    player.run_tick().await;
    assert!(player.advancement_done("minecraft:recipes/redstone/piston"));
    assert!(
        player.knows_recipe("minecraft:piston"),
        "revoking lets it unlock again"
    );

    assert!(recipe(&mut player, true, "minecraft:chest").await > 0);
    assert!(player.knows_recipe("minecraft:chest"));
    assert!(
        player.criterion_done("minecraft:recipes/decorations/chest", "has_the_recipe"),
        "giving a recipe meets recipe_unlocked"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_a_tab_selects_it_once() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = session_with_outbound(&world, 5);
    let select = tuff_vanilla_data::packet::play::Clientbound::SelectAdvancementsTab.id();
    let seen = |tab: &str| Serverbound::SeenAdvancements(Some(tab.to_owned()));
    player.handle_packet(seen("minecraft:story/root")).await;
    player.handle_packet(seen("minecraft:story/root")).await;
    let mut tabs = sent(&mut outbound, select);
    assert_eq!(tabs.len(), 1);
    assert!(tabs[0].read_bool().unwrap());
    assert_eq!(tabs[0].read_string().unwrap(), "minecraft:story/root");
    player
        .handle_packet(Serverbound::SeenAdvancements(None))
        .await;
    player
        .handle_packet(seen("minecraft:story/mine_stone"))
        .await;
    let mut tabs = sent(&mut outbound, select);
    assert_eq!(tabs.len(), 1);
    assert!(!tabs[0].read_bool().unwrap(), "only roots are tabs");
    world.shutdown().await;
}

fn facts(kind: tuff_vanilla_data::registry::EntityType) -> EntityFacts {
    EntityFacts {
        kind: Some(kind),
        ..EntityFacts::default()
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regions_report_kills_breeding_and_cures() {
    use crate::world::messages::AdvancementEvent;
    use tuff_vanilla_data::registry::EntityType;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 6);
    player.stand_at([0.5, 100.0, 0.5], true);
    let kill = crate::world::messages::Kill {
        kind: EntityType::Zombie,
        holder: "zombie".into(),
        victim: EntityFacts {
            position: [3.0, 100.0, 0.5],
            ..facts(EntityType::Zombie)
        },
        killing_blow: DamageFacts {
            kind: &tuff_vanilla_data::damage_type::PLAYER_ATTACK,
            source: Some(facts(EntityType::Player)),
            direct: Some(facts(EntityType::Player)),
        },
    };
    player
        .handle_event(PlayerEvent::KilledEntity(Box::new(kill)))
        .await;
    assert!(player.criterion_done("minecraft:adventure/kill_a_mob", "minecraft:zombie"));
    assert!(player.advancement_done("minecraft:adventure/kill_a_mob"));
    assert!(player.criterion_done("minecraft:adventure/root", "killed_something"));
    assert!(!player.criterion_done("minecraft:adventure/kill_all_mobs", "minecraft:skeleton"));

    let bred = AdvancementEvent::BredAnimals {
        parent: facts(EntityType::Cow),
        partner: facts(EntityType::Cow),
        child: Some(EntityFacts {
            flags: Flags {
                baby: true,
                ..Flags::default()
            },
            ..facts(EntityType::Cow)
        }),
    };
    player
        .handle_event(PlayerEvent::Advancement(Box::new(bred)))
        .await;
    assert!(player.advancement_done("minecraft:husbandry/breed_an_animal"));
    let animals_bred = || {
        player.stat(tuff_core::world::stats::Stat::custom(
            tuff_vanilla_data::registry::CustomStat::AnimalsBred,
        ))
    };
    assert_eq!(animals_bred(), 1);
    assert!(player.criterion_done("minecraft:husbandry/bred_all_animals", "minecraft:cow"));
    assert!(!player.criterion_done("minecraft:husbandry/bred_all_animals", "minecraft:pig"));

    let cured = AdvancementEvent::CuredZombieVillager {
        zombie: facts(EntityType::ZombieVillager),
        villager: facts(EntityType::Villager),
    };
    player
        .handle_event(PlayerEvent::Advancement(Box::new(cured)))
        .await;
    assert!(player.advancement_done("minecraft:story/cure_zombie_villager"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn placing_blocks_and_travelling_meet_their_criteria() {
    use crate::world::messages::{ActionOutcome, Placement};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 7);
    player.stand_at([0.5, 100.0, 0.5], true);
    let seeds = Inventory::hotbar_slot(0);
    give(&mut player, seeds, Item::WheatSeeds);
    let at = |block: Block, y: f64| LocationFacts {
        position: [0.5, y, 0.5],
        dimension: "minecraft:overworld".into(),
        biome: Some("minecraft:plains".into()),
        block: Some(block.default_state()),
        ..LocationFacts::default()
    };
    let placed = ActionOutcome::Placed {
        off_hand: false,
        item: Item::WheatSeeds,
        placement: Some(Box::new(Placement {
            used_on: at(Block::Farmland, 64.5),
            placed: Some(at(Block::Wheat, 65.5)),
        })),
    };
    player
        .handle_event(PlayerEvent::ActionResult {
            sequence: 1,
            outcome: placed,
        })
        .await;
    assert!(player.criterion_done("minecraft:husbandry/plant_seed", "wheat"));
    assert!(player.advancement_done("minecraft:husbandry/plant_seed"));

    let nether = player.shared().dimensions.by_key(THE_NETHER).unwrap().id;
    let overworld = player.shared().dimensions.by_key(OVERWORLD).unwrap().id;
    player.travel_for_test(nether, [0.5, 70.0, 0.5]);
    assert!(player.advancement_done("minecraft:story/enter_the_nether"));
    player.travel_for_test(overworld, [7_001.0, 70.0, 0.5]);
    assert!(player.advancement_done("minecraft:nether/fast_travel"));
    player.run_tick().await;
    assert_eq!(
        player.experience().total,
        100,
        "fast travel gives 100 points"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_location_trigger_reads_the_reported_biome() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 8);
    player.stand_at([0.5, 100.0, 0.5], true);
    player
        .handle_event(PlayerEvent::Surroundings {
            dimension: player.dimension_id(),
            seq: 0,
            in_water: false,
            submersion: Default::default(),
            in_lava: false,
            in_fire: None,
            climbable: false,
            climb_location: None,
            resets_fall: false,
            landed_on: None,
            portal: None,
            contact: Default::default(),
            biome: Some("minecraft:badlands"),
            stepping_on: None,
        })
        .await;
    while player.tick_count() % 20 != 19 {
        player.run_tick().await;
    }
    player.run_tick().await;
    assert!(player.criterion_done("minecraft:adventure/adventuring_time", "minecraft:badlands"));
    assert!(!player.criterion_done("minecraft:adventure/adventuring_time", "minecraft:desert"));
    world.shutdown().await;
}

/// Standing in water near spawn must not grant the "Remote Getaway" end
/// gateway advancement, since its enter_block criterion names a different
/// block.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enter_block_only_grants_the_advancement_for_its_own_block() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 9);
    player.stand_at([0.5, 100.0, 0.5], true);
    player
        .handle_event(PlayerEvent::Surroundings {
            dimension: player.dimension_id(),
            seq: 0,
            in_water: true,
            submersion: Default::default(),
            in_lava: false,
            in_fire: None,
            climbable: false,
            climb_location: None,
            resets_fall: false,
            landed_on: None,
            portal: None,
            contact: Default::default(),
            biome: Some("minecraft:plains"),
            stepping_on: None,
        })
        .await;
    assert!(!player.advancement_done("minecraft:end/enter_end_gateway"));

    let gateway = tuff_vanilla_data::Block::EndGateway.default_state();
    player.fire(crate::world::advancement::trigger::Event::EnterBlock(
        gateway,
    ));
    assert!(player.advancement_done("minecraft:end/enter_end_gateway"));
    world.shutdown().await;
}

fn write_pack(root: &std::path::Path, files: &[(&str, &str)]) {
    let pack = root.join("datapacks/adv");
    let current = tuff_vanilla_data::version::DATA_PACK_VERSION.major;
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::write(
        pack.join("pack.mcmeta"),
        format!(r#"{{"pack":{{"description":"","min_format":{current},"max_format":{current}}}}}"#),
    )
    .unwrap();
    for (path, contents) in files {
        let path = pack.join("data").join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}

const PACK_ROOT: &str = r#"{"display":{"icon":"minecraft:diamond","title":"Pack",
    "description":"A pack tab","background":"minecraft:block/stone"},
    "criteria":{"sneak":{"trigger":"minecraft:tick","conditions":{"player":"a:sneaking"}}}}"#;

fn pack_child(title: &str) -> String {
    format!(
        r#"{{"parent":"a:root","display":{{"icon":"minecraft:stone","title":"{title}",
            "description":"","announce_to_chat":false}},
            "criteria":{{"never":{{"trigger":"minecraft:impossible"}}}}}}"#
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pack_advancements_join_the_tree_and_a_reload_moves_progress_over() {
    let dir = TestDir::new();
    write_pack(
        dir.path(),
        &[
            (
                "a/predicate/sneaking.json",
                r#"{"type":"minecraft:entity_properties","entity":"this",
                    "predicate":{"minecraft:flags":{"is_sneaking":true}}}"#,
            ),
            ("a/advancement/root.json", PACK_ROOT),
            ("a/advancement/child.json", &pack_child("Child")),
        ],
    );
    let world = TestWorld::start(dir.path());
    let tree = world.shared.advancements();
    assert!(tree.id("a:root").is_some() && tree.id("a:child").is_some());
    assert!(tree.id("minecraft:story/root").is_some());
    assert!(world.shared.base_advancements.id("a:root").is_none());

    let (mut player, mut outbound) = session_with_outbound(&world, 1);
    player.stand_at([0.5, 100.0, 0.5], true);
    player.run_tick().await;
    assert!(!player.advancement_done("a:root"));
    player.pose.sneaking = true;
    player.run_tick().await;
    assert!(
        player.advancement_done("a:root"),
        "the stored predicate in the criterion was inlined"
    );
    let added: Vec<String> = updates(&mut outbound)
        .into_iter()
        .flat_map(|update| update.added)
        .map(|added| added.id)
        .collect();
    assert!(added.iter().any(|id| id == "a:child"), "{added:?}");
    assert_eq!(advancement(&mut player, true, &["a:child"], None).await, 1);

    std::fs::remove_file(
        dir.path()
            .join("datapacks/adv/data/a/advancement/child.json"),
    )
    .unwrap();
    write_pack(
        dir.path(),
        &[("a/advancement/extra.json", &pack_child("Extra"))],
    );
    let reload = crate::world::commands::console::run(world.shared.clone(), "reload").await;
    assert_eq!(reload.len(), 1);
    let reloaded = world.shared.advancements();
    assert!(!Arc::ptr_eq(&reloaded, &tree));
    assert!(reloaded.id("a:child").is_none() && reloaded.id("a:extra").is_some());

    player.run_tick().await;
    assert!(Arc::ptr_eq(&player.progress.advancement_tree, &reloaded));
    assert!(player.advancement_done("a:root"));
    assert!(!player.advancement_done("a:child"));
    let after = updates(&mut outbound);
    assert_eq!(after.len(), 1);
    assert!(after[0].reset, "a reload resends the whole tree");
    let added: Vec<&str> = after[0].added.iter().map(|a| a.id.as_str()).collect();
    assert!(added.contains(&"a:root") && added.contains(&"a:extra"));
    assert!(!added.contains(&"a:child"));
    assert!(after[0].progress.iter().any(|(id, criteria)| {
        id == "a:root"
            && criteria
                .iter()
                .any(|(name, time)| name == "sneak" && time.is_some())
    }));
    let saved = SavedProgress.save(&player).unwrap();
    assert!(saved.get_compound("a:root").is_some());
    assert!(saved.get_compound("a:child").is_none());

    let old = Arc::downgrade(&tree);
    drop(tree);
    assert_eq!(old.strong_count(), 0, "the previous tree is freed");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn levitating_and_falling_meet_their_criteria() {
    use crate::world::effects::ActiveEffect;
    use crate::world::player::Fall;
    use tuff_vanilla_data::registry::MobEffect;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 7);
    player.stand_at([0.5, 70.0, 0.5], false);
    player
        .add_effect(ActiveEffect::timed(MobEffect::Levitation, 400, 0))
        .await;
    player.tick_advancements().await;
    player.stand_at([0.5, 100.0, 0.5], false);
    player.tick_advancements().await;
    assert!(!player.advancement_done("minecraft:end/levitate"));
    player.stand_at([0.5, 121.0, 0.5], false);
    player.tick_advancements().await;
    assert!(player.advancement_done("minecraft:end/levitate"));

    let name = "minecraft:adventure/fall_from_world_height";
    player.body.effects = Default::default();
    player.stand_at([0.5, 320.0, 0.5], false);
    player.body.fall = Fall::new(1.0);
    player.tick_advancements().await;
    player.stand_at([0.5, -60.0, 0.5], true);
    player.body.fall = Fall::new(380.0);
    player.tick_advancements().await;
    assert!(!player.advancement_done(name), "still falling");
    player.body.fall = Fall::default();
    player.tick_advancements().await;
    assert!(player.advancement_done(name));
    world.shutdown().await;
}
