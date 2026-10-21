use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_item::text::TextComponent;
use tuff_vanilla_data::packet::play::Clientbound as Id;

struct Running {
    handle: tuff::ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    addr: std::net::SocketAddr,
}

async fn start(instance: &TempDir) -> Running {
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    Running { handle, task, addr }
}

async fn stop(server: Running) {
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

async fn run(server: &Running, line: &str) -> Vec<TextComponent> {
    server.handle.run_command(line).await.unwrap()
}

fn key(lines: &[TextComponent]) -> &str {
    lines
        .first()
        .and_then(|line| line.translate.as_deref())
        .unwrap_or_default()
}

/// The `index`th argument of the first line, as plain text.
fn arg(lines: &[TextComponent], index: usize) -> String {
    lines[0].with[index].plain()
}

async fn connect(server: &Running, name: &str) -> Bot {
    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new(name)
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot
}

fn received(bot: &Bot, id: Id) -> u64 {
    bot.state().received.get(&id.id()).copied().unwrap_or(0)
}

#[tuff_core::watchdog]
#[tokio::test]
async fn scoreboard_and_team_commands_reach_clients_and_persist() {
    let instance = TempDir::new();
    let server = start(&instance).await;
    let bot = connect(&server, "Tester").await;

    let added = run(&server, "scoreboard objectives add kills dummy \"Kills\"").await;
    assert_eq!(key(&added), "commands.scoreboard.objectives.add.success");
    assert_eq!(arg(&added, 0), "[Kills]");
    let again = run(&server, "scoreboard objectives add kills dummy").await;
    assert_eq!(key(&again), "commands.scoreboard.objectives.add.duplicate");
    let bad = run(&server, "scoreboard objectives add x nonsense").await;
    assert_eq!(key(&bad), "argument.criteria.invalid");
    let unknown = run(&server, "scoreboard players set Tester nope 1").await;
    assert_eq!(key(&unknown), "arguments.objective.notFound");
    run(&server, "scoreboard objectives add hp health").await;
    let read_only = run(&server, "scoreboard players set Tester hp 1").await;
    assert_eq!(key(&read_only), "arguments.objective.readonly");

    let set = run(&server, "scoreboard players set Tester kills 5").await;
    assert_eq!(key(&set), "commands.scoreboard.players.set.success.single");
    let added = run(&server, "scoreboard players add #bonus kills 3").await;
    assert_eq!(
        key(&added),
        "commands.scoreboard.players.add.success.single"
    );
    let operation = run(
        &server,
        "scoreboard players operation Tester kills += #bonus kills",
    )
    .await;
    assert_eq!(
        key(&operation),
        "commands.scoreboard.players.operation.success.single"
    );
    let div0 = run(
        &server,
        "scoreboard players operation Tester kills /= #none kills",
    )
    .await;
    assert_eq!(key(&div0), "arguments.operation.div0");
    let got = run(&server, "scoreboard players get Tester kills").await;
    assert_eq!(key(&got), "commands.scoreboard.players.get.success");
    assert_eq!(arg(&got, 1), "8");
    let missing = run(&server, "scoreboard players get Nobody kills").await;
    assert_eq!(key(&missing), "commands.scoreboard.players.get.null");

    let before = received(&bot, Id::SetScore);
    let shown = run(&server, "scoreboard objectives setdisplay sidebar kills").await;
    assert_eq!(key(&shown), "commands.scoreboard.objectives.display.set");
    let same = run(&server, "scoreboard objectives setdisplay sidebar kills").await;
    assert_eq!(
        key(&same),
        "commands.scoreboard.objectives.display.alreadySet"
    );
    bot.wait_for(|state| {
        state.received.contains_key(&Id::SetDisplayObjective.id())
            && state.received.get(&Id::SetScore.id()).copied().unwrap_or(0) >= before + 3
    })
    .await
    .unwrap();
    assert!(received(&bot, Id::SetObjective) >= 1);

    let team = run(&server, "team add red \"Red Team\"").await;
    assert_eq!(key(&team), "commands.team.add.success");
    run(&server, "team modify red color red").await;
    let unchanged = run(&server, "team modify red color red").await;
    assert_eq!(key(&unchanged), "commands.team.option.color.unchanged");
    let joined = run(&server, "team join red Tester").await;
    assert_eq!(key(&joined), "commands.team.join.success.single");
    bot.wait_for(|state| {
        state
            .received
            .get(&Id::SetPlayerTeam.id())
            .copied()
            .unwrap_or(0)
            >= 3
    })
    .await
    .unwrap();

    let pass =
        |lines: &[TextComponent]| key(lines).starts_with("commands.execute.conditional.pass");
    assert!(pass(&run(&server, "execute if entity @a[team=red]").await));
    assert!(!pass(
        &run(&server, "execute if entity @a[team=!red]").await
    ));
    assert!(pass(
        &run(&server, "execute if entity @a[scores={kills=8}]").await
    ));
    assert!(!pass(
        &run(&server, "execute if entity @a[scores={kills=9..}]").await
    ));
    assert!(pass(
        &run(&server, "execute if score Tester kills matches 5..").await
    ));
    assert!(pass(
        &run(&server, "execute if score Tester kills > #bonus kills").await
    ));
    assert!(!pass(
        &run(&server, "execute if score Tester kills = #missing kills").await
    ));
    assert!(pass(
        &run(
            &server,
            "execute if entity @a[nbt={Dimension:\"minecraft:overworld\"}]"
        )
        .await
    ));
    assert!(!pass(
        &run(
            &server,
            "execute if entity @a[nbt={Dimension:\"minecraft:the_nether\"}]"
        )
        .await
    ));

    run(
        &server,
        "execute store result score #stored kills run scoreboard players get Tester kills",
    )
    .await;
    let stored = run(&server, "scoreboard players get #stored kills").await;
    assert_eq!(arg(&stored, 1), "8");
    run(
        &server,
        "execute store success score #failed kills run scoreboard players get Nobody kills",
    )
    .await;
    let failed = run(&server, "scoreboard players get #failed kills").await;
    assert_eq!(arg(&failed, 1), "0");
    run(
        &server,
        "execute as @a store result score @s kills if entity @a",
    )
    .await;
    let counted = run(&server, "scoreboard players get Tester kills").await;
    assert_eq!(arg(&counted, 1), "1");

    run(&server, "scoreboard objectives add vote trigger").await;
    let unprimed = run(&server, "execute as Tester run trigger vote").await;
    assert_eq!(key(&unprimed), "commands.trigger.failed.unprimed");
    let enabled = run(&server, "scoreboard players enable Tester vote").await;
    assert_eq!(
        key(&enabled),
        "commands.scoreboard.players.enable.success.single"
    );
    let triggered = run(&server, "execute as Tester run trigger vote add 4").await;
    assert_eq!(key(&triggered), "commands.trigger.add.success");
    let locked = run(&server, "execute as Tester run trigger vote").await;
    assert_eq!(key(&locked), "commands.trigger.failed.unprimed");
    let not_trigger = run(&server, "scoreboard players enable Tester kills").await;
    assert_eq!(
        key(&not_trigger),
        "commands.scoreboard.players.enable.invalid"
    );

    let listed = run(&server, "scoreboard players list Tester").await;
    assert_eq!(
        key(&listed),
        "commands.scoreboard.players.list.entity.success"
    );
    drop(bot);
    stop(server).await;
    assert!(
        instance
            .path()
            .join("world/data/minecraft/scoreboard.dat")
            .is_file()
    );

    let server = start(&instance).await;
    let got = run(&server, "scoreboard players get Tester vote").await;
    assert_eq!(arg(&got, 1), "4");
    let members = run(&server, "team list red").await;
    assert_eq!(key(&members), "commands.team.list.members.success");
    assert_eq!(arg(&members, 2), "Tester");
    let bot = connect(&server, "Late").await;
    bot.wait_for(|state| {
        [Id::SetPlayerTeam, Id::SetObjective, Id::SetDisplayObjective]
            .iter()
            .all(|id| state.received.contains_key(&id.id()))
    })
    .await
    .unwrap();
    let removed = run(&server, "scoreboard objectives remove kills").await;
    assert_eq!(
        key(&removed),
        "commands.scoreboard.objectives.remove.success"
    );
    let emptied = run(&server, "team empty red").await;
    assert_eq!(key(&emptied), "commands.team.empty.success");
    drop(bot);
    stop(server).await;
}

/// Polls a score until `condition` holds; the session reports on its own
/// ticks, so there is no fixed moment to read it.
async fn wait_for_score(
    server: &Running,
    holder: &str,
    objective: &str,
    condition: impl Fn(i32) -> bool,
) -> i32 {
    let line = format!("scoreboard players get {holder} {objective}");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let lines = run(server, &line).await;
            if key(&lines) == "commands.scoreboard.players.get.success" {
                let value: i32 = arg(&lines, 1).parse().unwrap();
                if condition(value) {
                    return value;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the score never met the condition")
}

#[tuff_core::watchdog]
#[tokio::test]
async fn criteria_objectives_follow_play() {
    let instance = TempDir::new();
    let server = start(&instance).await;
    let bot = connect(&server, "Tester").await;
    run(&server, "scoreboard objectives add deaths deathCount").await;
    run(&server, "scoreboard objectives add lvl level").await;
    run(
        &server,
        "scoreboard objectives add time minecraft.custom:minecraft.play_time",
    )
    .await;
    let read_only = run(&server, "scoreboard players add Tester lvl 1").await;
    assert_eq!(key(&read_only), "arguments.objective.readonly");

    run(&server, "experience add Tester 5 levels").await;
    wait_for_score(&server, "Tester", "lvl", |level| level == 5).await;
    let played = wait_for_score(&server, "Tester", "time", |ticks| ticks > 0).await;
    wait_for_score(&server, "Tester", "time", |ticks| ticks > played).await;
    run(&server, "kill Tester").await;
    wait_for_score(&server, "Tester", "deaths", |deaths| deaths == 1).await;
    drop(bot);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn hidden_death_messages_reach_only_the_victims_teammates() {
    let instance = TempDir::new();
    let server = start(&instance).await;
    let victim = connect(&server, "Victim").await;
    let mate = connect(&server, "Mate").await;
    let stranger = connect(&server, "Stranger").await;
    let chats = |bot: &Bot| received(bot, Id::SystemChat);
    victim
        .wait_for(|s| s.received.get(&Id::SystemChat.id()).copied().unwrap_or(0) >= 3)
        .await
        .unwrap();
    mate.wait_for(|s| s.received.get(&Id::SystemChat.id()).copied().unwrap_or(0) >= 2)
        .await
        .unwrap();
    stranger
        .wait_for(|s| s.received.get(&Id::SystemChat.id()).copied().unwrap_or(0) >= 1)
        .await
        .unwrap();
    run(&server, "team add red").await;
    for name in ["Victim", "Mate"] {
        let joined = run(&server, &format!("team join red {name}")).await;
        assert_eq!(key(&joined), "commands.team.join.success.single");
    }
    let changed = run(
        &server,
        "team modify red deathMessageVisibility hideForOtherTeams",
    )
    .await;
    assert_eq!(
        key(&changed),
        "commands.team.option.deathMessageVisibility.success"
    );
    let (victim_before, mate_before, stranger_before) =
        (chats(&victim), chats(&mate), chats(&stranger));

    run(&server, "kill Victim").await;
    mate.wait_for(|s| s.received.get(&Id::SystemChat.id()).copied().unwrap_or(0) > mate_before)
        .await
        .unwrap();
    victim.wait_for(|s| s.deaths > 0).await.unwrap();
    let infos = |bot: &Bot| received(bot, Id::PlayerInfoUpdate);
    let (victim_infos, stranger_infos) = (infos(&victim), infos(&stranger));
    run(&server, "gamemode creative Mate").await;
    for (bot, before) in [(&victim, victim_infos), (&stranger, stranger_infos)] {
        bot.wait_for(|s| {
            s.received
                .get(&Id::PlayerInfoUpdate.id())
                .copied()
                .unwrap_or(0)
                > before
        })
        .await
        .unwrap();
    }
    assert_eq!(chats(&victim), victim_before, "the victim is not told");
    assert_eq!(
        chats(&stranger),
        stranger_before,
        "other teams are not told"
    );
    drop((victim, mate, stranger));
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn killed_entity_loses_scores_and_team() {
    let instance = TempDir::new();
    let server = start(&instance).await;
    run(&server, "forceload add 0 0").await;
    run(&server, "setblock 0 100 0 stone").await;
    run(
        &server,
        "summon armor_stand 0 101 0 {UUID:[I;1,2,3,4],Tags:[mark]}",
    )
    .await;
    let holder = "00000001-0000-0002-0000-000300000004";
    run(&server, "scoreboard objectives add n dummy").await;
    run(&server, "team add red").await;
    run(&server, &format!("scoreboard players set {holder} n 7")).await;
    run(&server, &format!("team join red {holder}")).await;
    let got = run(&server, &format!("scoreboard players get {holder} n")).await;
    assert!(!got.is_empty());
    let k = run(&server, "kill @e[tag=mark]").await;
    assert!(!k.is_empty());
    let mut gone = false;
    for _ in 0..100 {
        let left = run(&server, &format!("scoreboard players get {holder} n")).await;
        let team = run(&server, "team list red").await;
        if format!("{left:?}").contains("players.get.null")
            && format!("{team:?}").contains("members.empty")
        {
            gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let left = run(&server, &format!("scoreboard players get {holder} n")).await;
    let team = run(&server, "team list red").await;
    stop(server).await;
    assert!(gone, "{left:?} {team:?}");
}
