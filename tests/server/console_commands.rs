use crate::common;

use common::{MockSessionServer, start_server};
use serde_json::Value;

async fn run(server: &common::TestServer, line: &str) -> Vec<Value> {
    server
        .handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(|line| line.to_value())
        .collect()
}

fn key(line: &Value) -> &str {
    line["translate"].as_str().unwrap_or_default()
}

#[tuff_core::watchdog]
#[tokio::test]
async fn console_runs_world_settings_commands_with_vanilla_feedback() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let seed = run(&server, "seed").await;
    assert_eq!(key(&seed[0]), "commands.seed.success");
    let copy = &seed[0]["with"][0];
    assert_eq!(copy["translate"], "chat.square_brackets");
    let number = &copy["with"][0];
    let text = number["text"].as_str().unwrap();
    assert!(text.parse::<i64>().is_ok(), "{number}");
    assert_eq!(number["color"], "green");
    assert_eq!(number["click_event"]["action"], "copy_to_clipboard");
    assert_eq!(number["click_event"]["value"], text);
    assert_eq!(
        number["hover_event"]["value"]["translate"],
        "chat.copy.click"
    );
    assert_eq!(number["insertion"], text);

    let query = run(&server, "gamerule keep_inventory").await;
    assert_eq!(key(&query[0]), "commands.gamerule.query");
    assert_eq!(query[0]["with"][1]["text"], "false");
    let set = run(&server, "/gamerule keep_inventory true").await;
    assert_eq!(key(&set[0]), "commands.gamerule.set");
    let same = run(&server, "gamerule minecraft:keep_inventory true").await;
    assert_eq!(key(&same[0]), "commands.gamerule.not_set");
    assert_eq!(same[0]["color"], "red");
    let query = run(&server, "gamerule keep_inventory").await;
    assert_eq!(query[0]["with"][1]["text"], "true");
    let int = run(&server, "gamerule random_tick_speed 0").await;
    assert_eq!(key(&int[0]), "commands.gamerule.set");
    let low = run(&server, "gamerule random_tick_speed -1").await;
    assert_eq!(key(&low[0]), "argument.integer.low");

    let weather = run(&server, "weather rain 1000").await;
    assert_eq!(key(&weather[0]), "commands.weather.set.rain");
    let difficulty = run(&server, "difficulty hard").await;
    assert_eq!(key(&difficulty[0]), "commands.difficulty.success");
    let same = run(&server, "difficulty hard").await;
    assert_eq!(key(&same[0]), "commands.difficulty.failure");
    let query = run(&server, "difficulty").await;
    assert_eq!(key(&query[0]), "commands.difficulty.query");
    assert_eq!(query[0]["with"][0]["translate"], "options.difficulty.hard");

    let time = run(&server, "time set 1000").await;
    assert_eq!(key(&time[0]), "commands.time.set.absolute");
    let spawn = run(&server, "setworldspawn 10 70 20 90 0").await;
    assert_eq!(key(&spawn[0]), "commands.setworldspawn.success");
    assert_eq!(spawn[0]["with"][0]["text"], "10");
    assert_eq!(spawn[0]["with"][3]["text"], "90.0");

    let idle = run(&server, "setidletimeout 5").await;
    assert_eq!(key(&idle[0]), "commands.setidletimeout.success");
    assert_eq!(idle[0]["with"][0]["text"], "5");

    let list = run(&server, "list").await;
    assert_eq!(key(&list[0]), "commands.list.players");
    assert_eq!(list[0]["with"][0]["text"], "0");

    let nobody = run(&server, "give @a stone").await;
    assert_eq!(key(&nobody[0]), "argument.entity.notfound.player");
    assert_eq!(nobody[0]["color"], "red");

    let unknown = run(&server, "frobnicate now").await;
    assert_eq!(key(&unknown[0]), "command.unknown.command");
    assert_eq!(unknown[1]["extra"][0]["text"], "");
    assert_eq!(unknown[1]["extra"][1]["text"], "frobnicate now");
    let bad = run(&server, "gamemode hardcore").await;
    assert_eq!(key(&bad[0]), "argument.gamemode.invalid");
    assert_eq!(bad[0]["with"][0]["text"], "hardcore");

    let help = run(&server, "help").await;
    assert!(help.iter().any(|line| line["text"] == "/seed"));
    assert!(help.iter().any(|line| {
        line["text"]
            .as_str()
            .unwrap()
            .starts_with("/gamemode <gamemode>")
    }));
    let help_time = run(&server, "help time").await;
    assert!(
        help_time
            .iter()
            .any(|line| line["text"].as_str().unwrap().starts_with("/time set"))
    );

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
    let world = server._instance.path().join("world");
    let rules = std::fs::read(world.join("data/minecraft/game_rules.dat")).unwrap();
    assert!(!rules.is_empty());
    assert!(world.join("data/minecraft/weather.dat").is_file());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn console_changes_blocks_and_entities_in_loaded_regions() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let unloaded = run(&server, "setblock 3 300 3 minecraft:stone").await;
    assert_eq!(key(&unloaded[0]), "argument.pos.unloaded");
    let forced = run(&server, "forceload add 0 0 20 20").await;
    assert_eq!(key(&forced[0]), "commands.forceload.added.multiple");
    assert_eq!(forced[0]["with"][0]["text"], "4");
    assert_eq!(forced[0]["with"][2]["text"], "[0, 0]");
    let again = run(&server, "forceload add 0 0").await;
    assert_eq!(key(&again[0]), "commands.forceload.added.failure");
    let query = run(&server, "forceload query 18 18").await;
    assert_eq!(key(&query[0]), "commands.forceload.query.success");
    let listed = run(&server, "forceload query").await;
    assert_eq!(key(&listed[0]), "commands.forceload.list.multiple");
    let set = run(&server, "setblock 3 300 3 minecraft:stone").await;
    assert_eq!(key(&set[0]), "commands.setblock.success");
    let same = run(&server, "setblock 3 300 3 stone keep").await;
    assert_eq!(key(&same[0]), "commands.setblock.failed");
    let pass = run(&server, "execute if block 3 300 3 stone").await;
    assert_eq!(key(&pass[0]), "commands.execute.conditional.pass");
    let fail = run(&server, "execute if block 3 300 3 #logs").await;
    assert_eq!(key(&fail[0]), "commands.execute.conditional.fail");
    let unless = run(&server, "execute unless block 3 300 3 dirt").await;
    assert_eq!(key(&unless[0]), "commands.execute.conditional.pass");

    let fill = run(&server, "fill 0 300 0 2 302 2 oak_planks").await;
    assert_eq!(key(&fill[0]), "commands.fill.success");
    assert_eq!(fill[0]["with"][0]["text"], "27");
    let hollow = run(&server, "fill 0 300 0 2 302 2 oak_planks hollow").await;
    assert_eq!(fill[0]["with"][0]["text"], "27");
    assert_eq!(hollow[0]["with"][0]["text"], "1");
    let replaced = run(&server, "fill 0 300 0 2 302 2 stone replace #planks").await;
    assert_eq!(replaced[0]["with"][0]["text"], "26");
    let compare = run(&server, "execute if blocks 0 300 0 2 302 2 0 300 0 all").await;
    assert_eq!(key(&compare[0]), "commands.execute.conditional.pass_count");
    assert_eq!(compare[0]["with"][0]["text"], "27");
    let too_big = run(&server, "fill 0 0 0 100 100 100 stone").await;
    assert_eq!(key(&too_big[0]), "commands.fill.toobig");
    assert_eq!(too_big[0]["with"][0]["text"], "32768");
    let cloned = run(&server, "clone 0 300 0 2 302 2 10 300 10").await;
    assert_eq!(key(&cloned[0]), "commands.clone.success");
    assert_eq!(
        cloned[0]["with"][0]["text"], "26",
        "the air core did not change"
    );
    let overlap = run(&server, "clone 0 300 0 2 302 2 1 300 1").await;
    assert_eq!(key(&overlap[0]), "commands.clone.overlap");

    let summoned = run(
        &server,
        "summon item 5 310 5 {Item:{id:\"minecraft:stone\",count:3}}",
    )
    .await;
    assert_eq!(key(&summoned[0]), "commands.summon.success");
    let orb = run(
        &server,
        "summon minecraft:experience_orb 5 310 5 {Value:7s}",
    )
    .await;
    assert_eq!(key(&orb[0]), "commands.summon.success");
    let counted = run(
        &server,
        "execute if entity @e[type=item,x=5,y=310,z=5,distance=..1]",
    )
    .await;
    assert_eq!(key(&counted[0]), "commands.execute.conditional.pass_count");
    assert_eq!(counted[0]["with"][0]["text"], "1");
    let killed = run(&server, "kill @e[type=item,x=5,y=310,z=5,distance=..1]").await;
    assert_eq!(key(&killed[0]), "commands.kill.success.single");
    let tnt = run(&server, "summon tnt 5 310 5 {fuse:1000s,NoGravity:1b}").await;
    assert_eq!(key(&tnt[0]), "commands.summon.success");
    let primed = run(&server, "execute if entity @e[type=tnt]").await;
    assert_eq!(key(&primed[0]), "commands.execute.conditional.pass_count");
    let all = run(&server, "kill @e[x=5,y=310,z=5,distance=..16]").await;
    assert_eq!(key(&all[0]), "commands.kill.success.multiple");
    assert_eq!(all[0]["with"][0]["text"], "2");
    let none = run(&server, "kill @e[x=5,y=310,z=5,distance=..16]").await;
    assert_eq!(key(&none[0]), "argument.entity.notfound.entity");
    run(&server, "difficulty peaceful").await;
    let zombie = run(&server, "summon zombie 5 310 5").await;
    assert_eq!(key(&zombie[0]), "commands.summon.failed.peaceful");
    let named = run(
        &server,
        "summon pig 5 310 5 {CustomName:\"Bacon\",NoAI:1b,NoGravity:1b}",
    )
    .await;
    assert_eq!(key(&named[0]), "commands.summon.success");
    assert_eq!(named[0]["with"][0]["text"], "Bacon");
    run(&server, "kill @e[type=pig,x=5,y=310,z=5,distance=..2]").await;
    run(&server, "difficulty hard").await;
    let chained = run(
        &server,
        "execute positioned 3 300 3 run setblock ~ ~1 ~ dirt",
    )
    .await;
    assert_eq!(key(&chained[0]), "commands.setblock.success");
    assert_eq!(chained[0]["with"][1]["text"], "301");
    let over = run(
        &server,
        "execute positioned 3 0 3 positioned over world_surface run setblock ~ ~ ~ dirt",
    )
    .await;
    assert_eq!(key(&over[0]), "commands.setblock.success");
    assert_eq!(over[0]["with"][1]["text"], "302");
    let removed = run(&server, "forceload remove all").await;
    assert_eq!(key(&removed[0]), "commands.forceload.removed.all");
    let none = run(&server, "forceload query").await;
    assert_eq!(key(&none[0]), "commands.forceload.added.none");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn console_teleports_non_player_entities() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let forced = run(&server, "forceload add 0 0 110 110").await;
    assert_eq!(key(&forced[0]), "commands.forceload.added.multiple");
    // `summon` does not wait for a chunk to finish generating, unlike
    // `setblock`; pre-touch every position this test uses so summoning and
    // teleporting onto them is deterministic rather than racing worldgen.
    for pos in ["5 300 5", "50 300 50", "100 300 100"] {
        let ready = run(&server, &format!("setblock {pos} minecraft:glass")).await;
        assert_eq!(key(&ready[0]), "commands.setblock.success");
    }

    let item = run(
        &server,
        "summon item 5 310 5 {Item:{id:\"minecraft:stone\",count:1},NoGravity:1b}",
    )
    .await;
    assert_eq!(key(&item[0]), "commands.summon.success");
    let pig = run(&server, "summon pig 5 310 5 {NoAI:1b,NoGravity:1b}").await;
    assert_eq!(key(&pig[0]), "commands.summon.success");

    // A selector that only finds non-player entities must move them
    // (coordinates form) rather than fail with "no player was found".
    // Only the summoned item: with real terrain generated around it, the
    // forceloaded area can hold other dropped items too.
    let moved = run(
        &server,
        "tp @e[type=item,x=5,y=310,z=5,distance=..1] 50 310 50",
    )
    .await;
    assert_eq!(key(&moved[0]), "commands.teleport.success.location.single");
    // Bare integer coordinates snap to the block centre, so allow for the
    // resulting +0.5 offset on each axis instead of asserting an exact spot.
    let at_dest = run(
        &server,
        "execute if entity @e[type=item,x=50,y=310,z=50,distance=..1]",
    )
    .await;
    assert_eq!(key(&at_dest[0]), "commands.execute.conditional.pass_count");
    assert_eq!(at_dest[0]["with"][0]["text"], "1");

    // The entity-destination form, targeting a non-player too.
    let moved = run(
        &server,
        "tp @e[type=pig,x=5,y=310,z=5,distance=..1] @e[type=item,x=50,y=310,z=50,distance=..1,limit=1]",
    )
    .await;
    assert_eq!(key(&moved[0]), "commands.teleport.success.entity.single");
    let at_dest = run(
        &server,
        "execute if entity @e[type=pig,x=50,y=310,z=50,distance=..1]",
    )
    .await;
    assert_eq!(key(&at_dest[0]), "commands.execute.conditional.pass_count");
    assert_eq!(at_dest[0]["with"][0]["text"], "1");

    // A selector matching only non-players still moves all of them together
    // (scoped to this spot: generated terrain around it may hold others).
    let moved = run(&server, "tp @e[x=50,y=310,z=50,distance=..1] 100 310 100").await;
    assert_eq!(
        key(&moved[0]),
        "commands.teleport.success.location.multiple"
    );
    assert_eq!(moved[0]["with"][0]["text"], "2");
    let at_dest = run(
        &server,
        "execute if entity @e[x=100,y=310,z=100,distance=..1]",
    )
    .await;
    assert_eq!(key(&at_dest[0]), "commands.execute.conditional.pass_count");
    assert_eq!(at_dest[0]["with"][0]["text"], "2");

    // The general "nothing to move" error still fires once nothing matches.
    // (Only the item is checked here: a killed mob lingers through its
    // death animation for a moment, unlike an item, which vanishes at once.)
    let killed = run(&server, "kill @e[type=item,x=100,y=310,z=100,distance=..1]").await;
    assert_eq!(key(&killed[0]), "commands.kill.success.single");
    let empty = run(
        &server,
        "tp @e[type=item,x=100,y=310,z=100,distance=..1] 0 310 0",
    )
    .await;
    assert_eq!(key(&empty[0]), "argument.entity.notfound.entity");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn console_gives_and_clears_effects_on_non_player_entities() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let forced = run(&server, "forceload add 0 0").await;
    assert_eq!(key(&forced[0]), "commands.forceload.added.single");
    let ready = run(&server, "setblock 5 300 5 minecraft:glass").await;
    assert_eq!(key(&ready[0]), "commands.setblock.success");
    let pig = run(&server, "summon pig 5 310 5 {NoAI:1b,NoGravity:1b}").await;
    assert_eq!(key(&pig[0]), "commands.summon.success");
    let pigs = "@e[type=pig,x=5,y=310,z=5,distance=..1]";

    let given = run(&server, &format!("effect give {pigs} glowing 100")).await;
    assert_eq!(key(&given[0]), "commands.effect.give.success.single");
    let cleared = run(&server, &format!("effect clear {pigs} glowing")).await;
    assert_eq!(
        key(&cleared[0]),
        "commands.effect.clear.specific.success.single"
    );
    let again = run(&server, &format!("effect clear {pigs} glowing")).await;
    assert_eq!(key(&again[0]), "commands.effect.clear.specific.failed");
    let everything = run(&server, &format!("effect clear {pigs}")).await;
    assert_eq!(
        key(&everything[0]),
        "commands.effect.clear.everything.failed"
    );

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn console_locates_biomes_and_rejects_unknown_structures() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let biome = run(&server, "locate biome #minecraft:is_overworld").await;
    assert_eq!(key(&biome[0]), "commands.locate.biome.success");
    assert!(
        biome[0]["with"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("#minecraft:is_overworld (minecraft:")
    );
    let coordinates = &biome[0]["with"][1];
    assert_eq!(coordinates["color"], "green");
    assert_eq!(coordinates["click_event"]["action"], "suggest_command");

    let invalid = run(&server, "locate structure minecraft:no_such_structure").await;
    assert_eq!(key(&invalid[0]), "commands.locate.structure.invalid");
    assert_eq!(invalid[0]["color"], "red");
    let invalid_tag = run(&server, "locate structure #minecraft:no_such_tag").await;
    assert_eq!(key(&invalid_tag[0]), "commands.locate.structure.invalid");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn console_locates_points_of_interest_in_loaded_chunks() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let loaded = run(&server, "forceload add ~ ~").await;
    assert_eq!(key(&loaded[0]), "commands.forceload.added.single");
    let missing = run(&server, "locate poi minecraft:farmer").await;
    assert_eq!(key(&missing[0]), "commands.locate.poi.not_found");
    assert_eq!(missing[0]["color"], "red");
    let placed = run(&server, "setblock ~2 ~ ~ minecraft:composter").await;
    assert_eq!(key(&placed[0]), "commands.setblock.success");

    let found = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let found = run(&server, "locate poi minecraft:farmer").await;
            if key(&found[0]) == "commands.locate.poi.success" {
                return found;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the placed composter is indexed as a point of interest");
    assert_eq!(found[0]["with"][0]["text"], "minecraft:farmer");
    assert_eq!(found[0]["with"][2]["text"], "2");
    let tagged = run(&server, "locate poi #minecraft:acquirable_job_site").await;
    assert_eq!(
        tagged[0]["with"][0]["text"],
        "#minecraft:acquirable_job_site (minecraft:farmer)"
    );
    let other = run(&server, "locate poi minecraft:weaponsmith").await;
    assert_eq!(key(&other[0]), "commands.locate.poi.not_found");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

async fn game_time(server: &common::TestServer) -> i64 {
    let lines = run(server, "time query gametime").await;
    lines[0]["with"][0]["text"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// Polls `game_time` until `condition` holds, rather than guessing a sleep
/// duration for however fast this machine ticks.
async fn wait_for_game_time(server: &common::TestServer, condition: impl Fn(i64) -> bool) -> i64 {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let time = game_time(server).await;
            if condition(time) {
                return time;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("game time never met the condition")
}

#[tuff_core::watchdog]
#[tokio::test]
async fn console_runs_tick_commands_with_vanilla_feedback() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let query = run(&server, "tick query").await;
    assert_eq!(key(&query[0]), "commands.tick.status.running");
    assert_eq!(key(&query[1]), "commands.tick.query.rate.running");
    assert_eq!(query[1]["with"][0]["text"], "20.0");
    assert_eq!(key(&query[2]), "commands.tick.query.percentiles");

    let rate = run(&server, "tick rate 10").await;
    assert_eq!(key(&rate[0]), "commands.tick.rate.success");
    assert_eq!(rate[0]["with"][0]["text"], "10.0");

    let freeze = run(&server, "tick freeze").await;
    assert_eq!(key(&freeze[0]), "commands.tick.status.frozen");
    let frozen_at = game_time(&server).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        game_time(&server).await,
        frozen_at,
        "a frozen game time must not advance"
    );

    let query = run(&server, "tick query").await;
    assert_eq!(key(&query[0]), "commands.tick.status.frozen");

    let no_step_to_stop = run(&server, "tick step stop").await;
    assert_eq!(key(&no_step_to_stop[0]), "commands.tick.step.stop.fail");

    let step = run(&server, "tick step 3").await;
    assert_eq!(key(&step[0]), "commands.tick.step.success");
    assert_eq!(step[0]["with"][0]["text"], "3");
    let stepped_to = wait_for_game_time(&server, |t| t >= frozen_at + 3).await;
    assert_eq!(stepped_to, frozen_at + 3);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        game_time(&server).await,
        stepped_to,
        "stepping re-freezes once it is done"
    );

    let unfreeze = run(&server, "tick unfreeze").await;
    assert_eq!(key(&unfreeze[0]), "commands.tick.status.running");
    wait_for_game_time(&server, |t| t > stepped_to).await;

    let cannot_step = run(&server, "tick step").await;
    assert_eq!(key(&cannot_step[0]), "commands.tick.step.fail");

    let no_sprint_to_stop = run(&server, "tick sprint stop").await;
    assert_eq!(key(&no_sprint_to_stop[0]), "commands.tick.sprint.stop.fail");

    run(&server, "tick freeze").await;
    let before_sprint = game_time(&server).await;
    let sprint = run(&server, "tick sprint 20").await;
    assert_eq!(key(&sprint[0]), "commands.tick.status.sprinting");
    let sprinted_to = wait_for_game_time(&server, |t| t >= before_sprint + 20).await;
    assert_eq!(sprinted_to, before_sprint + 20);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        game_time(&server).await,
        sprinted_to,
        "the game re-freezes once a sprint that started frozen ends"
    );

    // A very long sprint is interrupted long before it could finish, and
    // reports the interruption to the command that stopped it (the
    // sprint's own completion report goes to the console log instead).
    let long_sprint = run(&server, "tick sprint 2000000000").await;
    assert_eq!(key(&long_sprint[0]), "commands.tick.status.sprinting");
    let stop = run(&server, "tick sprint stop").await;
    assert_eq!(key(&stop[0]), "commands.tick.sprint.stop.success");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

/// A barrier member that takes a while over every tick and records each
/// tick it saw.
struct Seen {
    ticks: std::sync::Mutex<Vec<u64>>,
    last: tokio::sync::watch::Sender<u64>,
}

struct SlowTicker(std::sync::Arc<Seen>);

impl tuff::feature::Feature for SlowTicker {
    fn id(&self) -> &'static str {
        "slow-ticker"
    }

    fn load(&self, cx: &mut tuff::feature::LoadContext) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            cx.registries()
                .services
                .push(std::sync::Arc::new(SlowTickerFactory(self.0.clone())));
        }
        Ok(())
    }
}

struct SlowTickerFactory(std::sync::Arc<Seen>);

impl tuff_core::world::services::ServiceFactory for SlowTickerFactory {
    fn create(
        &self,
        _: &tuff_core::world::dimension::Dimensions,
    ) -> std::sync::Arc<dyn tuff_core::world::services::WorldService> {
        std::sync::Arc::new(SlowTickerService(self.0.clone()))
    }
}

struct SlowTickerService(std::sync::Arc<Seen>);

impl tuff_core::world::services::WorldService for SlowTickerService {
    fn start(&self, shared: &std::sync::Arc<tuff_core::world::Shared>) {
        let seen = self.0.clone();
        let mut clock = shared.clock.clone();
        let member = shared.tick_barrier.join(*clock.borrow());
        tokio::spawn(async move {
            while clock.changed().await.is_ok() {
                let tick = *clock.borrow_and_update();
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                seen.ticks.lock().unwrap().push(tick);
                seen.last.send_replace(tick);
                member.finished(tick);
            }
        });
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn sprint_waits_for_every_actor_to_finish_each_tick() {
    let (_session_server, session_url) = MockSessionServer::start().await;
    let seen = std::sync::Arc::new(Seen {
        ticks: std::sync::Mutex::new(Vec::new()),
        last: tokio::sync::watch::channel(0).0,
    });
    let mut last = seen.last.subscribe();
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(SlowTicker(seen.clone())));
    let server = common::start_server_with_features(
        common::TempDir::new(),
        "127.0.0.1:0",
        &session_url,
        Some(false),
        features,
    )
    .await;

    run(&server, "tick freeze").await;
    let before = game_time(&server).await as u64;
    run(&server, "tick sprint 40").await;
    let after = before + 40;
    last.wait_for(|&tick| tick >= after).await.unwrap();
    assert_eq!(game_time(&server).await as u64, after);

    let sprinted: Vec<u64> = seen
        .ticks
        .lock()
        .unwrap()
        .iter()
        .copied()
        .filter(|&tick| tick > before)
        .collect();
    assert_eq!(
        sprinted,
        (before + 1..=before + 40).collect::<Vec<_>>(),
        "a slow actor must run every sprinted tick, not just the latest"
    );

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn console_turns_saving_off_and_on_and_saves_with_vanilla_feedback() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let keys = |lines: Vec<Value>| -> Vec<String> {
        lines.iter().map(|line| key(line).to_owned()).collect()
    };
    assert_eq!(
        keys(run(&server, "save-on").await),
        ["commands.save.alreadyOn"]
    );
    assert_eq!(
        keys(run(&server, "save-off").await),
        ["commands.save.disabled"]
    );
    let again = run(&server, "save-off").await;
    assert_eq!(keys(again.clone()), ["commands.save.alreadyOff"]);
    assert_eq!(again[0]["color"], "red");
    assert_eq!(
        keys(run(&server, "save-all").await),
        ["commands.save.saving", "commands.save.success"]
    );
    assert_eq!(
        keys(run(&server, "save-all flush").await),
        ["commands.save.saving", "commands.save.success"]
    );
    assert_eq!(
        keys(run(&server, "save-on").await),
        ["commands.save.enabled"]
    );

    let hold = server
        .handle
        .hold_saves("the nightly backup", std::time::Duration::from_secs(600))
        .await
        .unwrap()
        .unwrap();
    let refused = run(&server, "save-all flush").await;
    assert_eq!(key(&refused[0]), "commands.save.saving");
    assert_eq!(
        refused[1]["text"],
        "Saving is paused by the nightly backup until the backup finishes"
    );
    assert_eq!(refused[1]["color"], "red");
    hold.release().await;
    assert_eq!(
        keys(run(&server, "save-all").await),
        ["commands.save.saving", "commands.save.success"]
    );

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn worldborder_changes_the_live_border_and_saves_it() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;
    let first = |lines: Vec<Value>| lines.into_iter().next().unwrap();
    let text = |line: &Value, index: usize| line["with"][index]["text"].clone();

    let get = first(run(&server, "worldborder get").await);
    assert_eq!(key(&get), "commands.worldborder.get");
    assert_eq!(text(&get, 0), "59999968");
    let set = first(run(&server, "worldborder set 1000").await);
    assert_eq!(key(&set), "commands.worldborder.set.immediate");
    assert_eq!(text(&set, 0), "1000.0");
    for (line, expected) in [
        (
            "worldborder set 1000",
            "commands.worldborder.set.failed.nochange",
        ),
        (
            "worldborder set 0.5",
            "commands.worldborder.set.failed.small",
        ),
        (
            "worldborder add 59999968",
            "commands.worldborder.set.failed.big",
        ),
        (
            "worldborder center 30000000 0",
            "commands.worldborder.set.failed.far",
        ),
    ] {
        assert_eq!(key(&first(run(&server, line).await)), expected, "{line}");
    }
    let big = first(run(&server, "worldborder add 59999968").await);
    assert_eq!(text(&big, 0), "5.9999968E7");

    let center = first(run(&server, "worldborder center 10 20").await);
    assert_eq!(key(&center), "commands.worldborder.center.success");
    assert_eq!(
        (text(&center, 0), text(&center, 1)),
        ("10.50".into(), "20.50".into())
    );
    let amount = first(run(&server, "worldborder damage amount 0.5").await);
    assert_eq!(key(&amount), "commands.worldborder.damage.amount.success");
    assert_eq!(text(&amount, 0), "0.50");
    let buffer = first(run(&server, "worldborder damage buffer 3").await);
    assert_eq!(key(&buffer), "commands.worldborder.damage.buffer.success");
    let distance = first(run(&server, "worldborder warning distance 7").await);
    assert_eq!(
        key(&distance),
        "commands.worldborder.warning.distance.success"
    );
    assert_eq!(text(&distance, 0), "7");
    let time = first(run(&server, "worldborder warning time 2s").await);
    assert_eq!(key(&time), "commands.worldborder.warning.time.success");
    assert_eq!(text(&time, 0), "2.00");
    let grow = first(run(&server, "worldborder set 2000 1d").await);
    assert_eq!(key(&grow), "commands.worldborder.set.grow");
    assert_eq!(
        (text(&grow, 0), text(&grow, 1)),
        ("2000.0".into(), "1200.00".into())
    );

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
    let file = server
        ._instance
        .path()
        .join("world/dimensions/minecraft/overworld/data/minecraft/world_border.dat");
    let saved = tuff_storage::document::inner(tuff_storage::files::read_nbt_file(&file).unwrap());
    assert_eq!(saved.get_double("center_x"), Some(10.5));
    assert_eq!(saved.get_double("center_z"), Some(20.5));
    assert_eq!(saved.get_double("damage_per_block"), Some(0.5));
    assert_eq!(saved.get_double("safe_zone"), Some(3.0));
    assert_eq!(saved.get_int("warning_blocks"), Some(7));
    assert_eq!(saved.get_int("warning_time"), Some(40));
    assert_eq!(saved.get_double("lerp_target"), Some(2000.0));
    let left = saved.get_long("lerp_time").unwrap();
    assert!((1..=24_000).contains(&left), "{left}");
    let size = saved.get_double("size").unwrap();
    assert!((1000.0..2000.0).contains(&size), "{size}");

    let server =
        common::start_server_in(server._instance, "127.0.0.1:0", &session_url, Some(false)).await;
    for (line, expected) in [
        (
            "worldborder center 10 20",
            "commands.worldborder.center.failed",
        ),
        (
            "worldborder damage amount 0.5",
            "commands.worldborder.damage.amount.failed",
        ),
        (
            "worldborder warning distance 7",
            "commands.worldborder.warning.distance.failed",
        ),
        ("worldborder set 2000", "commands.worldborder.set.immediate"),
    ] {
        assert_eq!(key(&first(run(&server, line).await)), expected, "{line}");
    }
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn console_runs_random_and_stopwatch_commands() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let sample = run(&server, "random value 5..5").await;
    assert_eq!(key(&sample[0]), "commands.random.error.range_too_small");
    let wide = run(&server, "random value -2000000000..2000000000").await;
    assert_eq!(key(&wide[0]), "commands.random.error.range_too_large");
    let value = run(&server, "random value 3..7").await;
    assert_eq!(key(&value[0]), "commands.random.sample.success");
    let first = run(&server, "random value 0..1000000 tuff:a").await;
    run(&server, "random reset tuff:a").await;
    let again = run(&server, "random value 0..1000000 tuff:a").await;
    assert_eq!(first[0]["with"], again[0]["with"]);
    let cleared = run(&server, "random reset *").await;
    assert_eq!(key(&cleared[0]), "commands.random.reset.all.success");

    let missing = run(&server, "stopwatch query tuff:t").await;
    assert_eq!(key(&missing[0]), "commands.stopwatch.does_not_exist");
    let created = run(&server, "stopwatch create tuff:t").await;
    assert_eq!(key(&created[0]), "commands.stopwatch.create.success");
    let twice = run(&server, "stopwatch create tuff:t").await;
    assert_eq!(key(&twice[0]), "commands.stopwatch.already_exists");
    let query = run(&server, "stopwatch query tuff:t").await;
    assert_eq!(key(&query[0]), "commands.stopwatch.query");
    let within = run(
        &server,
        "execute if stopwatch tuff:t 0..100 run random value 1..2",
    )
    .await;
    assert!(!within.is_empty());
    let gone = run(
        &server,
        "execute if stopwatch tuff:t 1000.. run random value 1..2",
    )
    .await;
    assert!(gone.is_empty(), "{gone:?}");
    let restart = run(&server, "stopwatch restart tuff:t").await;
    assert_eq!(key(&restart[0]), "commands.stopwatch.restart.success");
    let removed = run(&server, "stopwatch remove tuff:t").await;
    assert_eq!(key(&removed[0]), "commands.stopwatch.remove.success");
    let after = run(
        &server,
        "execute if stopwatch tuff:t 0..1 run random value 1..2",
    )
    .await;
    assert_eq!(key(&after[0]), "commands.stopwatch.does_not_exist");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn enchant_reaches_the_main_hand_of_a_mob() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    run(&server, "difficulty normal").await;
    run(&server, "forceload add 0 0 20 20").await;
    for pos in ["5 300 5", "7 300 7", "9 300 9"] {
        run(&server, &format!("setblock {pos} minecraft:glass")).await;
    }
    run(
        &server,
        "summon zombie 5 310 5 {NoAI:1b,NoGravity:1b,PersistenceRequired:1b,Tags:[\"armed\"],equipment:{mainhand:{id:\"minecraft:iron_sword\",count:1}}}",
    )
    .await;
    run(
        &server,
        "summon zombie 7 310 7 {NoAI:1b,NoGravity:1b,PersistenceRequired:1b,Tags:[\"bare\"]}",
    )
    .await;
    run(
        &server,
        "summon pig 9 310 9 {NoAI:1b,NoGravity:1b,Tags:[\"pig\"]}",
    )
    .await;

    let bare = run(
        &server,
        "enchant @e[type=zombie,x=7,y=310,z=7,distance=..2] sharpness 2",
    )
    .await;
    assert_eq!(key(&bare[0]), "commands.enchant.failed.itemless");
    let incompatible = run(
        &server,
        "enchant @e[type=zombie,x=5,y=310,z=5,distance=..2] protection 1",
    )
    .await;
    assert_eq!(
        key(&incompatible[0]),
        "commands.enchant.failed.incompatible"
    );
    let done = run(
        &server,
        "enchant @e[type=zombie,x=5,y=310,z=5,distance=..2] sharpness 2",
    )
    .await;
    assert_eq!(key(&done[0]), "commands.enchant.success.single");
    let held = run(
        &server,
        "execute if entity @e[type=zombie,x=5,y=310,z=5,distance=..2,nbt={equipment:{mainhand:{components:{\"minecraft:enchantments\":{\"minecraft:sharpness\":2}}}}}] run random value 1..2",
    )
    .await;
    assert!(!held.is_empty(), "{held:?}");
    let pig = run(
        &server,
        "enchant @e[type=pig,x=9,y=310,z=9,distance=..2] sharpness 1",
    )
    .await;
    assert_eq!(key(&pig[0]), "commands.enchant.failed.itemless");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn rotate_and_swing_reach_mobs() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    run(&server, "forceload add 0 0 20 20").await;
    for pos in ["5 300 5", "7 300 7"] {
        run(&server, &format!("setblock {pos} minecraft:glass")).await;
    }
    run(
        &server,
        "summon pig 5 310 5 {NoAI:1b,NoGravity:1b,PersistenceRequired:1b}",
    )
    .await;
    run(
        &server,
        "summon item 7 310 7 {Item:{id:\"minecraft:stone\",count:1},NoGravity:1b}",
    )
    .await;
    let pig = "@e[type=pig,x=5,y=310,z=5,distance=..2,limit=1]";
    let item = "@e[type=item,x=7,y=310,z=7,distance=..2,limit=1]";

    let rotated = run(&server, &format!("rotate {pig} 90 15")).await;
    assert_eq!(key(&rotated[0]), "commands.rotate.success");
    let turned = run(
        &server,
        "execute if entity @e[type=pig,nbt={Rotation:[90.0f,15.0f]}]",
    )
    .await;
    assert_eq!(key(&turned[0]), "commands.execute.conditional.pass_count");

    let swung = run(&server, &format!("swing {pig} offhand stab 10")).await;
    assert_eq!(key(&swung[0]), "commands.swing.success.single");
    let bad = run(&server, &format!("swing {pig} mainhand bogus")).await;
    assert_eq!(key(&bad[0]), "argument.enum.invalid");
    let still = run(&server, &format!("swing {item}")).await;
    assert_eq!(key(&still[0]), "commands.swing.failed.notliving");
}
