//! Opt-in differential test: the item entities that /fill, /setblock and
//! /clone leave behind on tuff must match the vanilla server's. Ignored, and
//! also needs `TUFF_VANILLA_TESTS=1` and `TUFF_ACCEPT_MOJANG_EULA=1`.

use crate::vanilla;

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use anyhow::Result;
use tuff::Server;
use vanilla::{Settings, Vanilla, VanillaOptions};

const SETTLE: Duration = Duration::from_millis(700);
const SEED: i64 = 1;

const FLOOR_Y: i32 = 199;

enum Target {
    Tuff(tuff::ServerHandle),
    Vanilla(vanilla::Rcon),
}

impl Target {
    fn name(&self) -> &'static str {
        match self {
            Self::Tuff(_) => "tuff",
            Self::Vanilla(_) => "vanilla",
        }
    }

    async fn run(&self, line: &str) -> Result<String> {
        match self {
            Self::Tuff(handle) => Ok(handle
                .run_command(line)
                .await?
                .iter()
                .map(|line| line.plain())
                .collect::<Vec<_>>()
                .join("\n")),
            Self::Vanilla(rcon) => rcon.command(line).await,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Drop {
    id: String,
    count: u32,
    pos: [f64; 3],
}

/// Items drift and get pushed out of blocks after popping, so only total
/// counts per id are compared; positions stay in the failure report. Seeds
/// and chorus fruit are random drops in vanilla and are not compared.
fn same_drops(a: &[Drop], b: &[Drop]) -> bool {
    let key = |d: &[Drop]| {
        let mut totals = std::collections::BTreeMap::<String, u32>::new();
        for d in d.iter().filter(|d| {
            !matches!(
                d.id.as_str(),
                "minecraft:wheat_seeds" | "minecraft:chorus_fruit"
            )
        }) {
            *totals.entry(d.id.clone()).or_default() += d.count;
        }
        totals
    };
    key(a) == key(b)
}

fn parse_items(text: &str) -> Vec<Drop> {
    let mut out = Vec::new();
    for part in text.split(" has the following entity data: ").skip(1) {
        let Some(pos) = part.find("Pos: [") else {
            continue;
        };
        let nums: Vec<f64> = part[pos + 6..]
            .split(']')
            .next()
            .unwrap_or("")
            .split(',')
            .filter_map(|n| n.trim().trim_end_matches('d').parse().ok())
            .collect();
        let Some(item) = part.find("Item: ") else {
            continue;
        };
        let item = &part[item..];
        let id = item
            .find("id: \"")
            .map(|at| item[at + 5..].split('"').next().unwrap_or("").to_owned())
            .unwrap_or_default();
        let count = item
            .find("count: ")
            .map(|at| {
                item[at + 7..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .unwrap_or(1);
        if nums.len() == 3 {
            out.push(Drop {
                id,
                count,
                pos: [nums[0], nums[1], nums[2]],
            });
        }
    }
    out.sort_by(|a, b| (&a.id, a.count).cmp(&(&b.id, b.count)));
    out
}

struct Fixture {
    name: &'static str,
    setup: Vec<String>,
}

fn sb(x: i32, y: i32, z: i32, block: &str) -> String {
    format!("setblock {x} {y} {z} {block}")
}

fn fixtures() -> Vec<Fixture> {
    let f = |name, setup: Vec<String>| Fixture { name, setup };
    let on = |support: &str, rows: &[(i32, &str)]| {
        let mut v = vec![sb(3, 200, 3, support)];
        v.extend(rows.iter().map(|(y, b)| sb(3, *y, 3, b)));
        v
    };
    let item = |slot: i32, id: &str| format!("{{Slot:{slot}b,id:\"minecraft:{id}\",count:3}}");
    vec![
        f(
            "big_dripleaf",
            on("dirt", &[(201, "big_dripleaf[facing=north]")]),
        ),
        f(
            "big_dripleaf_stem",
            on(
                "dirt",
                &[
                    (201, "big_dripleaf_stem[facing=north]"),
                    (202, "big_dripleaf[facing=north]"),
                ],
            ),
        ),
        f(
            "small_dripleaf",
            on(
                "dirt",
                &[
                    (201, "small_dripleaf[half=lower]"),
                    (202, "small_dripleaf[half=upper]"),
                ],
            ),
        ),
        f(
            "tall_grass",
            on(
                "grass_block",
                &[
                    (201, "tall_grass[half=lower]"),
                    (202, "tall_grass[half=upper]"),
                ],
            ),
        ),
        f(
            "sunflower",
            on(
                "grass_block",
                &[
                    (201, "sunflower[half=lower]"),
                    (202, "sunflower[half=upper]"),
                ],
            ),
        ),
        f(
            "oak_door",
            on(
                "stone",
                &[
                    (201, "oak_door[half=lower]"),
                    (202, "oak_door[half=upper]"),
                ],
            ),
        ),
        f("torch", on("stone", &[(201, "torch")])),
        f(
            "wall_torch",
            vec![
                sb(4, 201, 3, "stone"),
                sb(5, 201, 3, "wall_torch[facing=east]"),
                sb(3, 201, 3, "wall_torch[facing=west]"),
            ],
        ),
        f("rail", on("stone", &[(201, "rail")])),
        f("redstone_wire", on("stone", &[(201, "redstone_wire")])),
        f("repeater", on("stone", &[(201, "repeater")])),
        f("carpet", on("stone", &[(201, "white_carpet")])),
        f("oak_sign", on("stone", &[(201, "oak_sign")])),
        f(
            "oak_wall_sign",
            vec![
                sb(4, 201, 3, "stone"),
                sb(5, 201, 3, "oak_wall_sign[facing=east]"),
            ],
        ),
        f(
            "hanging_sign",
            vec![
                sb(3, 202, 3, "oak_planks"),
                sb(3, 201, 3, "oak_hanging_sign"),
            ],
        ),
        f(
            "vine",
            vec![sb(4, 201, 3, "stone"), sb(3, 201, 3, "vine[east=true]")],
        ),
        f(
            "kelp",
            vec![
                sb(3, 200, 3, "sand"),
                "fill 3 201 3 3 203 3 water".into(),
                sb(3, 201, 3, "kelp"),
                sb(3, 202, 3, "kelp"),
            ],
        ),
        f(
            "sugar_cane",
            vec![
                sb(2, 200, 3, "water"),
                sb(3, 200, 3, "sand"),
                sb(3, 201, 3, "sugar_cane"),
                sb(3, 202, 3, "sugar_cane"),
            ],
        ),
        f(
            "cactus",
            on("sand", &[(201, "cactus"), (202, "cactus")]),
        ),
        f(
            "bamboo",
            on("dirt", &[(201, "bamboo"), (202, "bamboo")]),
        ),
        f(
            "chorus",
            on(
                "end_stone",
                &[(201, "chorus_plant"), (202, "chorus_flower")],
            ),
        ),
        f("scaffolding", on("stone", &[(201, "scaffolding")])),
        f(
            "bed",
            vec![
                sb(3, 200, 3, "red_bed[part=foot,facing=east]"),
                sb(4, 200, 3, "red_bed[part=head,facing=east]"),
            ],
        ),
        f(
            "lantern",
            vec![
                sb(3, 202, 3, "stone"),
                sb(3, 201, 3, "lantern[hanging=true]"),
            ],
        ),
        f(
            "pointed_dripstone",
            vec![
                sb(3, 202, 3, "dripstone_block"),
                sb(3, 201, 3, "pointed_dripstone[vertical_direction=down]"),
            ],
        ),
        f(
            "gravel_above",
            on("stone", &[(201, "stone"), (202, "gravel")]),
        ),
        f(
            "sand_above_top",
            vec![
                sb(3, 203, 3, "stone"),
                sb(3, 204, 3, "sand"),
                sb(3, 205, 3, "sand"),
            ],
        ),
        f(
            "chest",
            vec![format!(
                "setblock 3 200 3 chest{{Items:[{},{}]}}",
                item(0, "diamond"),
                item(5, "apple")
            )],
        ),
        f(
            "barrel",
            vec![format!(
                "setblock 3 200 3 barrel{{Items:[{}]}}",
                item(0, "diamond")
            )],
        ),
        f(
            "hopper",
            vec![format!(
                "setblock 3 200 3 hopper{{Items:[{}]}}",
                item(0, "diamond")
            )],
        ),
        f(
            "furnace",
            vec![format!(
                "setblock 3 200 3 furnace{{Items:[{},{}]}}",
                item(0, "iron_ore"),
                item(1, "coal")
            )],
        ),
        f(
            "lectern",
            vec![
                "setblock 3 200 3 lectern[has_book=true]{Book:{id:\"minecraft:writable_book\",count:1}}"
                    .into(),
            ],
        ),
        f(
            "decorated_pot",
            vec!["setblock 3 200 3 decorated_pot{item:{id:\"minecraft:apple\",count:2}}".into()],
        ),
        f(
            "flower_pot",
            vec![sb(3, 200, 3, "potted_poppy")],
        ),
        f(
            "jukebox",
            vec![
                "setblock 3 200 3 jukebox[has_record=true]{RecordItem:{id:\"minecraft:music_disc_13\",count:1}}"
                    .into(),
            ],
        ),
    ]
}

struct Boxes;

impl Boxes {
    const FULL: &'static str = "2 200 2 4 203 4";
    const SUPPORT: &'static str = "2 200 2 4 200 4";
    const TOP: &'static str = "2 201 2 4 203 4";
}

fn commands() -> Vec<String> {
    let mut out = Vec::new();
    for b in [Boxes::FULL, Boxes::SUPPORT, Boxes::TOP] {
        for tail in [
            "air",
            "stone",
            "air destroy",
            "stone destroy",
            "air keep",
            "stone keep",
            "air hollow",
            "air outline",
            "air strict",
            "stone replace air",
            "air replace stone",
        ] {
            out.push(format!("fill {b} {tail}"));
        }
    }
    for pos in ["3 200 3", "3 201 3"] {
        for tail in [
            "air",
            "air replace",
            "air destroy",
            "air keep",
            "air strict",
            "stone",
            "stone destroy",
            "stone strict",
        ] {
            out.push(format!("setblock {pos} {tail}"));
        }
    }
    for mode in ["replace", "move"] {
        out.push(format!("clone 10 200 2 12 203 4 2 200 2 {mode}"));
        out.push(format!("clone 10 200 2 12 200 4 2 200 2 {mode}"));
        out.push(format!("clone 10 201 2 12 203 4 2 201 2 {mode}"));
    }
    out
}

async fn on_both(targets: &[&Target], line: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for t in targets {
        out.push(t.run(line).await?);
    }
    Ok(out)
}

async fn reset(target: &Target, fixture: &Fixture, clone_source: bool) -> Result<()> {
    target.run("gamerule block_drops false").await?;
    target.run("fill -4 199 -4 16 210 10 air").await?;
    target.run("kill @e[type=item]").await?;
    target
        .run(&format!("fill -4 {FLOOR_Y} -4 16 {FLOOR_Y} 10 stone"))
        .await?;
    for line in &fixture.setup {
        target.run(line).await?;
    }
    if clone_source {
        target.run("fill 10 200 2 12 203 4 stone").await?;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    target.run("kill @e[type=item]").await?;
    target.run("gamerule block_drops true").await?;
    Ok(())
}

async fn drops(target: &Target) -> Result<Vec<Drop>> {
    let text = target
        .run("execute as @e[type=item] run data get entity @s")
        .await?;
    if std::env::var("TUFF_DROPS_RAW").is_ok() {
        eprintln!("RAW {text}");
    }
    Ok(parse_items(&text))
}

async fn run_case(
    target: &Target,
    fixture: &Fixture,
    command: &str,
    clone: bool,
) -> (String, Vec<Drop>) {
    let ctx = format!("{} on {} | {command}", target.name(), fixture.name);
    reset(target, fixture, clone).await.expect(&ctx);
    let reply = target.run(command).await.expect(&ctx);
    tokio::time::sleep(SETTLE).await;
    (reply, drops(target).await.expect(&ctx))
}

fn opted_in() -> bool {
    if std::env::var("TUFF_VANILLA_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set TUFF_VANILLA_TESTS=1 to start a vanilla server");
        return false;
    }
    assert!(
        vanilla::env_accepts(std::env::var(vanilla::EULA_ENV).ok().as_deref()),
        "set TUFF_ACCEPT_MOJANG_EULA=1 after reading https://aka.ms/MinecraftEULA"
    );
    true
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a vanilla server; set TUFF_VANILLA_TESTS=1 and TUFF_ACCEPT_MOJANG_EULA=1"]
async fn fill_setblock_clone_drops_match_vanilla() {
    if !opted_in() {
        return;
    }
    let only = std::env::var("TUFF_DROPS_FILTER").ok();
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("vanilla-drops");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tuff")).unwrap();
    std::fs::write(
        dir.join("tuff").join("server.properties"),
        "pause-when-empty-seconds=-1\n",
    )
    .unwrap();
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(dir.join("tuff"))
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(4)
        .seed(SEED)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let options = VanillaOptions {
        accept_eula: true,
        ..Default::default()
    };
    let settings = Settings {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 0,
        rcon_port: 0,
        rcon_password: String::new(),
        seed: SEED,
        view_distance: 4,
        max_players: 2,
    };
    let vanilla = Vanilla::start(&options, settings, &dir.join("vanilla"))
        .await
        .unwrap();
    let tuff = Target::Tuff(handle.clone());
    let van = Target::Vanilla(vanilla.rcon());
    let both = [&tuff, &van];
    on_both(&both, "forceload add -4 -4 16 26").await.unwrap();
    on_both(&both, "gamerule random_tick_speed 0")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    let mut failures = Vec::new();
    let mut ran = 0;
    for fixture in fixtures() {
        for command in commands() {
            let key = format!("{} | {command}", fixture.name);
            if only
                .as_deref()
                .is_some_and(|f| !f.split(',').any(|p| key.contains(p)))
            {
                continue;
            }
            let clone = command.starts_with("clone");
            let (a, b) = tokio::join!(
                run_case(&tuff, &fixture, &command, clone),
                run_case(&van, &fixture, &command, clone)
            );
            let results = [a, b];
            ran += 1;
            let same = same_drops(&results[0].1, &results[1].1);
            if !same {
                failures.push(format!(
                    "{key}\n  tuff:    {:?}\n  vanilla: {:?}\n  replies: {:?} / {:?}",
                    results[0].1, results[1].1, results[0].0, results[1].0
                ));
            }
        }
    }
    vanilla.stop().await.unwrap();
    handle.shutdown().await.ok();
    let _ = task.await;
    eprintln!("{ran} cases, {} divergences", failures.len());
    assert!(
        failures.is_empty(),
        "{} divergences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
