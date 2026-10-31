// SPDX-License-Identifier: MIT
//! `/bots`: operator commands for using the bots plugin without another
//! plugin. Bots made here belong to the plugin itself and remember which
//! player commands them. The logic is in [`run`], free of host calls.

use std::collections::BTreeMap;

use tuff_bots_api::control::{
    BlockPos, BotId, BranchSpec, BuildBlock, BuildSpec, ControlError, FarmSpec, FlySpec, FollowSpec,
    GetToBlockSpec, Goal, Heading, LeafGoal, Location, MineSpec, Model, NearGoal, Policy, Process,
    RespawnPolicy, Skill, SpawnSpec, Vec3, XzGoal,
};

use tuff_plugin_sdk::command::{Node, Suggest, argument, command, literal, parser};
use tuff_plugin_sdk::prelude::{Ctx, Outcome, Player, arg};

use crate::core::{Bots, Host};

/// The owner id of bots made by commands.
pub const OWNER: &str = "bots";
pub const FOLLOW_RANGE: f64 = 3.0;
pub const MINE_RADIUS: u32 = 48;
pub const MAX_MINE_COUNT: u32 = 4096;
pub const MINE_LEVELS: std::ops::RangeInclusive<i32> = -64..=319;
pub const PRESETS: [&str; 3] = ["novice", "average", "expert"];
const MAX_NAME: usize = 16;

/// Who ran the command.
pub struct Sender {
    pub uuid: String,
    pub dimension: String,
    pub pos: Vec3,
    /// Holds `bots.command.admin`: may command bots made by others.
    pub admin: bool,
}

pub enum Shape {
    Box {
        size: [i32; 3],
        block: String,
        hollow: bool,
    },
    Line {
        axis: String,
        length: i32,
        state: String,
    },
}

/// Blocks a build starts this far east of the commander's feet.
pub const BUILD_GAP: i32 = 2;

/// A block state like `cobblestone` or `oak_stairs[facing=east,half=top]`,
/// namespaced.
pub fn block_state(text: &str) -> Result<String, String> {
    let bad = || "give a block like stone or oak_stairs[facing=east,half=top]".to_owned();
    let (name, props) = match text.split_once('[') {
        Some((name, rest)) => (name, Some(rest.strip_suffix(']').ok_or_else(bad)?)),
        None => (text, None),
    };
    let name = name.trim_start_matches("minecraft:");
    let word = |w: &str| {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    if !word(name) {
        return Err(bad());
    }
    if let Some(props) = props {
        let ok = props
            .split(',')
            .all(|kv| kv.split_once('=').is_some_and(|(k, v)| word(k) && word(v)));
        if !ok {
            return Err(bad());
        }
    }
    Ok(format!(
        "minecraft:{name}{}",
        props.map_or(String::new(), |p| format!("[{p}]"))
    ))
}

pub enum Cmd {
    Build(String, Shape),
    Spawn(String),
    Remove(String),
    Goto(String, Vec<i32>),
    Follow(String, String),
    Fly(String, i32, i32),
    Come(String),
    Mine(String, String, u32, Option<BranchSpec>),
    Find(String, String),
    Farm(String, Vec<i32>),
    Stop(String),
    List,
    Skill(String, String),
}

struct Entry {
    id: BotId,
    commander: String,
}

#[derive(Default)]
pub struct Registry {
    named: BTreeMap<String, Entry>,
    failed: BTreeMap<BotId, String>,
}

impl Registry {
    pub fn names(&self) -> Vec<String> {
        self.named.keys().cloned().collect()
    }
}

/// Comma separated block names, with or without the namespace.
pub fn block_names(text: &str) -> Result<Vec<String>, String> {
    let names: Vec<String> = text
        .split(',')
        .map(|n| n.trim().trim_start_matches("minecraft:").to_owned())
        .collect();
    let valid = |n: &String| {
        !n.is_empty()
            && n.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    if names.iter().all(valid) {
        Ok(names)
    } else {
        Err("give block names like iron_ore or coal_ore,deepslate_coal_ore".into())
    }
}

/// Rockets a `/bots fly` flight may spend.
const FLY_FIREWORKS: u32 = 64;

pub fn goto_goal(numbers: &[i32]) -> Result<Goal, String> {
    match *numbers {
        [y] => Ok(Goal::Leaf(LeafGoal::YLevel(y))),
        [x, z] => Ok(Goal::Leaf(LeafGoal::Xz(XzGoal { x, z }))),
        [x, y, z] => Ok(Goal::Leaf(LeafGoal::Block(BlockPos { x, y, z }))),
        _ => Err("goto takes <y>, <x z> or <x y z>".into()),
    }
}

pub fn skill_preset(name: &str) -> Option<Skill> {
    let (reaction_ticks, aim_error_degrees, turn_rate, click_cps_cap, attack_charge) =
        match name.to_ascii_lowercase().as_str() {
            "novice" => (10, 8.0, 12.0, 5.0, 0.5),
            "average" => (5, 3.0, 25.0, 8.0, 0.8),
            "expert" => (1, 0.5, 60.0, 12.0, 0.95),
            _ => return None,
        };
    Some(Skill {
        reaction_ticks,
        aim_error_degrees,
        turn_rate,
        click_cps_cap,
        attack_charge,
    })
}

pub fn valid_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn spawn_spec(name: &str, at: &Sender) -> SpawnSpec {
    SpawnSpec {
        name: name.to_owned(),
        model: Model::Classic,
        location: Location {
            dimension: Some(at.dimension.clone()),
            pos: at.pos.clone(),
            yaw: None,
            pitch: None,
        },
        skill: skill_preset("average").expect("a preset"),
        policy: Policy {
            allow_break: false,
            allow_place: false,
            allow_sprint: true,
            allow_parkour: false,
            max_fall: 3,
            break_allow: Vec::new(),
            break_deny: Vec::new(),
            throwaway_items: Vec::new(),
            avoid: Vec::new(),
            auto_eat: true,
            defend: true,
        },
        respawn: RespawnPolicy::Auto(20),
    }
}

fn describe(error: ControlError) -> String {
    match error {
        ControlError::UnknownBot => "that bot is gone".into(),
        ControlError::NotOwner => "not the bots plugin's bot".into(),
        ControlError::LimitReached => "too many bots".into(),
        ControlError::NameTaken => "that name is taken by a player".into(),
        ControlError::UnknownDimension => "unknown dimension".into(),
        ControlError::Invalid(reason) => reason,
        ControlError::Unavailable => "bots are unavailable right now".into(),
    }
}

fn bot_of(reg: &mut Registry, bots: &mut Bots, name: &str, by: &Sender) -> Result<BotId, String> {
    let Some(entry) = reg.named.get(name) else {
        return Err(format!("no bot named {name}"));
    };
    if bots.state(OWNER, entry.id) == Err(ControlError::UnknownBot) {
        reg.named.remove(name);
        return Err(format!("no bot named {name}"));
    }
    if entry.commander != by.uuid && !by.admin {
        return Err(format!("{name} is not your bot"));
    }
    Ok(entry.id)
}

fn describe_process(process: &Process) -> &'static str {
    match process {
        Process::Idle => "idle",
        Process::Goto(_) => "going",
        Process::Follow(_) => "following",
        Process::Explore(_) => "exploring",
        Process::Mine(_) => "mining",
        Process::GetToBlock(_) => "finding a block",
        Process::Break(_) => "breaking",
        Process::Place(_) => "placing",
        Process::Build(_) => "building",
        Process::Attack(_) => "attacking",
        Process::Farm(_) => "farming",
        Process::Shoot(_) => "shooting",
        Process::UseItem(_) => "using an item",
        Process::Fly(_) => "flying",
    }
}

/// The spec for `blocks` relative to the commander's position.
pub fn build_spec(by: &Sender, blocks: Vec<BuildBlock>) -> BuildSpec {
    BuildSpec {
        origin: BlockPos {
            x: by.pos.x.floor() as i32 + BUILD_GAP,
            y: by.pos.y.floor() as i32,
            z: by.pos.z.floor() as i32,
        },
        blocks,
    }
}

pub fn start_build(
    reg: &mut Registry,
    bots: &mut Bots,
    name: &str,
    by: &Sender,
    blocks: Vec<BuildBlock>,
) -> Result<(), String> {
    let id = bot_of(reg, bots, name, by)?;
    bots.set_process(OWNER, id, Process::Build(build_spec(by, blocks)))
        .map_err(describe)
}

/// A path inside the plugin's file store: relative, `/` separated.
pub fn file_path(path: &str) -> Result<String, String> {
    let path = path.trim();
    let bad = path.is_empty()
        || path.len() > 200
        || path.starts_with('/')
        || path.contains(['\\', ':'])
        || path.split('/').any(|s| matches!(s, "" | "." | ".."));
    if bad {
        return Err("give a relative file path like houses/hut.txt".into());
    }
    Ok(path.to_owned())
}

pub fn run(
    cmd: Cmd,
    by: &Sender,
    reg: &mut Registry,
    bots: &mut Bots,
    host: &mut impl Host,
) -> Result<String, String> {
    match &cmd {
        Cmd::Spawn(name) => {
            if !valid_name(name) {
                return Err("bot names are 1 to 16 letters, digits or underscores".into());
            }
            let _ = bot_of(reg, bots, name, by);
            if reg.named.contains_key(name) {
                return Err(format!("a bot named {name} exists"));
            }
            let id = bots
                .spawn(host, OWNER, spawn_spec(name, by))
                .map_err(describe)?;
            reg.named.insert(
                name.to_owned(),
                Entry {
                    id,
                    commander: by.uuid.clone(),
                },
            );
            Ok(format!("Spawned {name}"))
        }
        Cmd::Remove(name) => {
            let id = bot_of(reg, bots, name, by)?;
            bots.remove(host, OWNER, id).map_err(describe)?;
            reg.named.remove(name);
            Ok(format!("Removed {name}"))
        }
        Cmd::Goto(name, numbers) => {
            let goal = goto_goal(numbers)?;
            let id = bot_of(reg, bots, name, by)?;
            bots.set_process(OWNER, id, Process::Goto(goal))
                .map_err(describe)?;
            Ok(format!("{name} is on its way"))
        }
        Cmd::Fly(name, x, z) => {
            let id = bot_of(reg, bots, name, by)?;
            let spec = FlySpec {
                x: *x,
                z: *z,
                fireworks: FLY_FIREWORKS,
            };
            bots.set_process(OWNER, id, Process::Fly(spec))
                .map_err(describe)?;
            Ok(format!("{name} is taking off"))
        }
        Cmd::Follow(name, target) => {
            let id = bot_of(reg, bots, name, by)?;
            let spec = FollowSpec {
                target: target.to_owned(),
                range: FOLLOW_RANGE,
            };
            bots.set_process(OWNER, id, Process::Follow(spec))
                .map_err(describe)?;
            Ok(format!("{name} is following"))
        }
        Cmd::Come(name) => {
            let id = bot_of(reg, bots, name, by)?;
            let here = BlockPos {
                x: by.pos.x.floor() as i32,
                y: by.pos.y.floor() as i32,
                z: by.pos.z.floor() as i32,
            };
            let goal = Goal::Leaf(LeafGoal::Near(NearGoal {
                pos: here,
                radius: 1.5,
            }));
            bots.set_process(OWNER, id, Process::Goto(goal))
                .map_err(describe)?;
            Ok(format!("{name} is coming"))
        }
        Cmd::Mine(name, blocks, count, branch) => {
            if !(1..=MAX_MINE_COUNT).contains(count) {
                return Err(format!("count is 1 to {MAX_MINE_COUNT}"));
            }
            if let Some(level) = branch.as_ref().and_then(|b| b.level)
                && !MINE_LEVELS.contains(&level)
            {
                return Err(format!(
                    "y is {} to {}",
                    MINE_LEVELS.start(),
                    MINE_LEVELS.end()
                ));
            }
            let blocks = block_names(blocks)?;
            let id = bot_of(reg, bots, name, by)?;
            let spec = MineSpec {
                blocks,
                count: *count,
                radius: MINE_RADIUS,
                branch: branch.clone(),
            };
            bots.set_process(OWNER, id, Process::Mine(spec))
                .map_err(describe)?;
            Ok(format!("{name} is mining"))
        }
        Cmd::Build(name, shape) => {
            let (blocks, what) = match shape {
                Shape::Box {
                    size: [dx, dy, dz],
                    block,
                    hollow,
                } => (
                    crate::build::box_blocks(*dx, *dy, *dz, &block_state(block)?, *hollow)?,
                    "a box",
                ),
                Shape::Line {
                    axis,
                    length,
                    state,
                } => (
                    crate::build::line_blocks(axis, *length, &block_state(state)?)?,
                    "a line",
                ),
            };
            start_build(reg, bots, name, by, blocks)?;
            Ok(format!("{name} is building {what}"))
        }
        Cmd::Farm(name, n) => {
            let [x1, y1, z1, x2, y2, z2] = n[..] else {
                return Err("farm takes <x1 y1 z1 x2 y2 z2>".into());
            };
            let id = bot_of(reg, bots, name, by)?;
            let spec = FarmSpec {
                min: BlockPos {
                    x: x1.min(x2),
                    y: y1.min(y2),
                    z: z1.min(z2),
                },
                max: BlockPos {
                    x: x1.max(x2),
                    y: y1.max(y2),
                    z: z1.max(z2),
                },
                replant: true,
            };
            bots.set_process(OWNER, id, Process::Farm(spec))
                .map_err(describe)?;
            Ok(format!("{name} is farming"))
        }
        Cmd::Find(name, blocks) => {
            let blocks = block_names(blocks)?;
            let id = bot_of(reg, bots, name, by)?;
            let spec = GetToBlockSpec {
                blocks,
                radius: MINE_RADIUS,
            };
            bots.set_process(OWNER, id, Process::GetToBlock(spec))
                .map_err(describe)?;
            Ok(format!("{name} is looking for a block"))
        }
        Cmd::Stop(name) => {
            let id = bot_of(reg, bots, name, by)?;
            bots.stop(OWNER, id).map_err(describe)?;
            reg.failed.remove(&id);
            Ok(format!("{name} stopped"))
        }
        Cmd::Skill(name, preset) => {
            let skill = skill_preset(preset)
                .ok_or_else(|| format!("presets are {}", PRESETS.join(", ")))?;
            let id = bot_of(reg, bots, name, by)?;
            bots.set_skill(OWNER, id, skill).map_err(describe)?;
            Ok(format!("{name} is now {preset}"))
        }
        Cmd::List => {
            let names = reg.names();
            let mut lines = Vec::new();
            for name in names {
                let Ok(id) = bot_of(reg, bots, &name, by) else {
                    continue;
                };
                if let Ok(state) = bots.state(OWNER, id) {
                    let failed = match (&state.process, reg.failed.get(&id)) {
                        (Process::Idle, Some(reason)) => format!(" (failed: {reason})"),
                        _ => String::new(),
                    };
                    lines.push(format!(
                        "{name}: {}{failed} at {} {} {}",
                        describe_process(&state.process),
                        state.position.x.floor() as i32,
                        state.position.y.floor() as i32,
                        state.position.z.floor() as i32
                    ));
                }
            }
            Ok(if lines.is_empty() {
                "No bots".into()
            } else {
                lines.join("\n")
            })
        }
    }
}

thread_local! {
    static REGISTRY: std::cell::RefCell<Registry> = std::cell::RefCell::new(Registry::default());
}

/// Remembers why a command bot's last process failed, for `/bots list`.
pub fn note_event(bot: BotId, event: &crate::core::Event) {
    use crate::core::Event;
    REGISTRY.with(|reg| {
        let Ok(mut reg) = reg.try_borrow_mut() else {
            return;
        };
        match event {
            Event::ProcessFailed(reason) => {
                reg.failed.insert(bot, reason.clone());
            }
            Event::ProcessDone | Event::Removed => {
                reg.failed.remove(&bot);
            }
            _ => {}
        }
    });
}

fn hex(uuid: &tuff_plugin_sdk::types::Uuid) -> String {
    format!("{:016x}{:016x}", uuid.high, uuid.low)
}

fn sender_of(ctx: &Ctx) -> Result<Sender, String> {
    let player = ctx.player()?;
    let uuid = hex(&player.uuid);
    let at = tuff_plugin_sdk::players::location(&uuid).ok_or("you are not in a world")?;
    Ok(Sender {
        uuid,
        dimension: at.dimension,
        pos: Vec3 {
            x: at.pos.x,
            y: at.pos.y,
            z: at.pos.z,
        },
        admin: tuff_plugin_sdk::world::acting::has_permission("bots.command.admin")
            .unwrap_or(false),
    })
}

fn execute(ctx: &Ctx, cmd: impl FnOnce(&Ctx) -> Result<Cmd, String>) -> Outcome {
    let by = sender_of(ctx)?;
    let cmd = cmd(ctx)?;
    let text = crate::with(|bots, host| {
        REGISTRY.with(|reg| run(cmd, &by, &mut reg.borrow_mut(), bots, host))
    })?;
    ctx.reply(text).map_err(|e| format!("{e:?}"))?;
    Ok(1)
}

fn build_file(ctx: &Ctx) -> Outcome {
    let by = sender_of(ctx)?;
    let name: String = ctx.get("name")?;
    let path = file_path(&ctx.get::<String>("path")?)?;
    let replier = ctx.replier();
    crate::with(|bots, _| REGISTRY.with(|reg| bot_of(&mut reg.borrow_mut(), bots, &name, &by)))?;
    ctx.reply(format!("{name} is reading {path}"))
        .map_err(|e| format!("{e:?}"))?;
    tuff_plugin_sdk::exec::spawn(async move {
        let text = match tuff_plugin_sdk::files::read_string(&path).await {
            Ok(text) => text,
            Err(e) => {
                let _ = replier.reply(&format!("cannot read {path}: {e:?}"));
                return;
            }
        };
        let answer = crate::build::parse_blocks(&text).and_then(|blocks| {
            let count = blocks.len();
            crate::with(|bots, _| {
                REGISTRY.with(|reg| start_build(&mut reg.borrow_mut(), bots, &name, &by, blocks))
            })
            .map(|()| format!("{name} is building {count} blocks from {path}"))
        });
        let _ = replier.reply(&answer.unwrap_or_else(|e| format!("{path}: {e}")));
    });
    Ok(1)
}

fn names(_: &Suggest) -> Vec<String> {
    REGISTRY.with(|reg| reg.borrow().names())
}

fn mine_cmd(c: &Ctx, counted: bool, branch: Option<BranchSpec>) -> Result<Cmd, String> {
    let count = if counted {
        u32::try_from(c.get::<i32>("count")?).map_err(|_| "count must be positive".to_owned())?
    } else {
        16
    };
    Ok(Cmd::Mine(c.get("name")?, c.get("blocks")?, count, branch))
}

fn heading_nodes(counted: bool, level: bool) -> Vec<Node> {
    [
        ("north", Heading::North),
        ("south", Heading::South),
        ("east", Heading::East),
        ("west", Heading::West),
    ]
    .into_iter()
    .map(|(word, heading)| {
        literal(word).executes(move |ctx| {
            execute(&ctx, |c| {
                let level = if level { Some(c.get::<i32>("level")?) } else { None };
                mine_cmd(c, counted, Some(BranchSpec { level, heading }))
            })
        })
    })
    .collect()
}

fn mine_tail(node: Node, counted: bool) -> Node {
    let mut node = node.executes(move |ctx| execute(&ctx, |c| mine_cmd(c, counted, None)));
    let mut level = argument("level", parser::integer()).executes(move |ctx| {
        execute(&ctx, |c| {
            let level = Some(c.get::<i32>("level")?);
            mine_cmd(
                c,
                counted,
                Some(BranchSpec {
                    level,
                    heading: Heading::East,
                }),
            )
        })
    });
    for n in heading_nodes(counted, true) {
        level = level.then(n);
    }
    node = node.then(literal("y").then(level));
    for n in heading_nodes(counted, false) {
        node = node.then(n);
    }
    node
}

fn named(literal_name: &str, permission: &str) -> Node {
    literal(literal_name).requires(permission, 2)
}

fn on_bot(word: &str, make: impl Fn(String) -> Cmd + Clone + 'static) -> Node {
    let make = move |ctx: &Ctx| Ok(make(ctx.get::<String>("name")?));
    named(word, &format!("bots.command.{word}")).then(
        argument("name", parser::word())
            .suggests(names)
            .executes(move |ctx| execute(&ctx, &make)),
    )
}

fn build_box(ctx: &Ctx, hollow: bool) -> Outcome {
    execute(ctx, |c| {
        Ok(Cmd::Build(
            c.get("name")?,
            Shape::Box {
                size: [c.get("dx")?, c.get("dy")?, c.get("dz")?],
                block: c.get("block")?,
                hollow,
            },
        ))
    })
}

pub fn register() -> Result<(), String> {
    let number = |name: &str| argument(name, parser::integer());
    let goto = |ctx: Ctx, count: usize| {
        execute(&ctx, |ctx| {
            let numbers: Vec<i32> = ["a", "b", "c"][..count]
                .iter()
                .map(|n| ctx.get::<i32>(n))
                .collect::<Result<_, _>>()?;
            Ok(Cmd::Goto(ctx.get::<String>("name")?, numbers))
        })
    };
    command("bots")
        .then(
            named("spawn", "bots.command.spawn").then(
                argument("name", parser::word())
                    .executes(|ctx| execute(&ctx, |c| Ok(Cmd::Spawn(c.get("name")?)))),
            ),
        )
        .then(on_bot("remove", Cmd::Remove))
        .then(
            named("goto", "bots.command.goto").then(
                argument("name", parser::word()).suggests(names).then(
                    number("a").executes(move |ctx| goto(ctx, 1)).then(
                        number("b")
                            .executes(move |ctx| goto(ctx, 2))
                            .then(number("c").executes(move |ctx| goto(ctx, 3))),
                    ),
                ),
            ),
        )
        .then(named("follow", "bots.command.follow").then(
            argument("name", parser::word()).suggests(names).then(
                arg::<Player>("target").executes(|ctx| {
                    execute(&ctx, |c| {
                        let target = hex(&c.get::<Player>("target")?.uuid);
                        Ok(Cmd::Follow(c.get("name")?, target))
                    })
                }),
            ),
        ))
        .then(
            named("fly", "bots.command.fly").then(
                argument("name", parser::word())
                    .suggests(names)
                    .then(number("x").then(number("z").executes(|ctx| {
                        execute(&ctx, |c| {
                            Ok(Cmd::Fly(c.get("name")?, c.get("x")?, c.get("z")?))
                        })
                    }))),
            ),
        )
        .then(on_bot("come", Cmd::Come))
        .then(
            named("mine", "bots.command.mine").then(
                argument("name", parser::word()).suggests(names).then(
                    mine_tail(argument("blocks", parser::word()), false)
                        .then(mine_tail(argument("count", parser::integer()), true)),
                ),
            ),
        )
        .then(
            named("build", "bots.command.build").then(
                argument("name", parser::word())
                    .suggests(names)
                    .then(
                        literal("box").then(
                            number("dx").then(
                                number("dy").then(
                                    number("dz").then(
                                        argument("block", parser::word())
                                            .executes(|ctx| build_box(&ctx, false))
                                            .then(
                                                literal("hollow")
                                                    .executes(|ctx| build_box(&ctx, true)),
                                            ),
                                    ),
                                ),
                            ),
                        ),
                    )
                    .then(
                        literal("file").then(
                            argument("path", parser::greedy()).executes(|ctx| build_file(&ctx)),
                        ),
                    )
                    .then(literal("line").then(argument("axis", parser::word()).then(
                        number("length").then(argument("state", parser::greedy()).executes(
                            |ctx| {
                                execute(&ctx, |c| {
                                    Ok(Cmd::Build(
                                        c.get("name")?,
                                        Shape::Line {
                                            axis: c.get("axis")?,
                                            length: c.get("length")?,
                                            state: c.get("state")?,
                                        },
                                    ))
                                })
                            },
                        )),
                    ))),
            ),
        )
        .then(
            named("farm", "bots.command.farm").then(
                argument("name", parser::word()).suggests(names).then(
                    (1..=6)
                        .rev()
                        .fold(None::<Node>, |inner, i| {
                            let key = ["a", "b", "c", "d", "e", "f"][i - 1];
                            let mut node = number(key);
                            node = match inner {
                                Some(next) => node.then(next),
                                None => node.executes(|ctx| {
                                    execute(&ctx, |c| {
                                        let n: Vec<i32> = ["a", "b", "c", "d", "e", "f"]
                                            .iter()
                                            .map(|k| c.get::<i32>(k))
                                            .collect::<Result<_, _>>()?;
                                        Ok(Cmd::Farm(c.get("name")?, n))
                                    })
                                }),
                            };
                            Some(node)
                        })
                        .unwrap(),
                ),
            ),
        )
        .then(named("find", "bots.command.find").then(
            argument("name", parser::word()).suggests(names).then(
                argument("blocks", parser::word()).executes(|ctx| {
                    execute(&ctx, |c| Ok(Cmd::Find(c.get("name")?, c.get("blocks")?)))
                }),
            ),
        ))
        .then(on_bot("stop", Cmd::Stop))
        .then(named("list", "bots.command.list").executes(|ctx| execute(&ctx, |_| Ok(Cmd::List))))
        .then(
            named("skill", "bots.command.skill").then(
                argument("name", parser::word()).suggests(names).then(
                    argument("preset", parser::word())
                        .suggests(|_| PRESETS.iter().map(|p| (*p).to_owned()).collect())
                        .executes(|ctx| {
                            execute(&ctx, |c| Ok(Cmd::Skill(c.get("name")?, c.get("preset")?)))
                        }),
                ),
            ),
        )
        .help()
        .register()
        .map_err(|e| format!("{e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Recording;

    fn sender(uuid: &str, admin: bool) -> Sender {
        Sender {
            uuid: uuid.into(),
            dimension: "minecraft:overworld".into(),
            pos: Vec3 {
                x: 5.5,
                y: 64.0,
                z: 5.5,
            },
            admin,
        }
    }

    struct Rig {
        bots: Bots,
        host: Recording,
        reg: Registry,
    }

    impl Rig {
        fn new() -> Self {
            let mut host = Recording::default();
            host.loaded.push(OWNER.into());
            Self {
                bots: Bots::default(),
                host,
                reg: Registry::default(),
            }
        }

        fn run(&mut self, cmd: Cmd, by: &Sender) -> Result<String, String> {
            run(cmd, by, &mut self.reg, &mut self.bots, &mut self.host)
        }

        fn process(&mut self, name: &str) -> Process {
            let id = self.reg.named[name].id;
            self.bots.state(OWNER, id).unwrap().process
        }
    }

    #[test]
    fn farm_normalises_the_box_and_replants() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        rig.run(Cmd::Spawn("Bob".into()), &op).unwrap();
        rig.run(Cmd::Farm("Bob".into(), vec![5, 70, 5, 1, 69, 2]), &op)
            .unwrap();
        let Process::Farm(spec) = rig.process("Bob") else {
            panic!("not farming");
        };
        assert_eq!(spec.min, BlockPos { x: 1, y: 69, z: 2 });
        assert_eq!(spec.max, BlockPos { x: 5, y: 70, z: 5 });
        assert!(spec.replant);
        assert!(rig.run(Cmd::Farm("Bob".into(), vec![1, 2]), &op).is_err());
    }

    #[test]
    fn goto_takes_y_xz_or_xyz() {
        assert_eq!(goto_goal(&[70]), Ok(Goal::Leaf(LeafGoal::YLevel(70))));
        assert_eq!(
            goto_goal(&[1, 2]),
            Ok(Goal::Leaf(LeafGoal::Xz(XzGoal { x: 1, z: 2 })))
        );
        assert_eq!(
            goto_goal(&[1, 2, 3]),
            Ok(Goal::Leaf(LeafGoal::Block(BlockPos { x: 1, y: 2, z: 3 })))
        );
        assert!(goto_goal(&[]).is_err());
        assert!(goto_goal(&[1, 2, 3, 4]).is_err());
    }

    #[test]
    fn mine_and_find_set_their_processes_and_check_input() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        rig.run(Cmd::Spawn("Bob".into()), &op).unwrap();
        rig.run(
            Cmd::Mine("Bob".into(), "minecraft:iron_ore,coal_ore".into(), 3, None),
            &op,
        )
        .unwrap();
        let Process::Mine(spec) = rig.process("Bob") else {
            panic!("not mining");
        };
        assert_eq!(spec.blocks, ["iron_ore", "coal_ore"]);
        assert_eq!(spec.count, 3);
        rig.run(Cmd::Find("Bob".into(), "diamond_ore".into()), &op)
            .unwrap();
        assert!(matches!(rig.process("Bob"), Process::GetToBlock(_)));
        assert!(
            rig.run(Cmd::Mine("Bob".into(), "Iron Ore".into(), 3, None), &op)
                .is_err()
        );
        assert!(
            rig.run(Cmd::Mine("Bob".into(), "iron_ore".into(), 0, None), &op)
                .is_err()
        );
        assert!(rig.run(Cmd::Find("Bob".into(), "".into()), &op).is_err());
    }

    #[test]
    fn mine_takes_a_branch_plan_and_rejects_levels_out_of_range() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        rig.run(Cmd::Spawn("Bob".into()), &op).unwrap();
        let plan = |level| {
            Some(BranchSpec {
                level,
                heading: Heading::North,
            })
        };
        rig.run(
            Cmd::Mine("Bob".into(), "iron_ore".into(), 1, plan(Some(-50))),
            &op,
        )
        .unwrap();
        let Process::Mine(spec) = rig.process("Bob") else {
            panic!("not mining");
        };
        assert_eq!(spec.branch, plan(Some(-50)));
        for bad in [-65, 320] {
            assert!(
                rig.run(
                    Cmd::Mine("Bob".into(), "iron_ore".into(), 1, plan(Some(bad))),
                    &op
                )
                .is_err()
            );
        }
    }

    #[test]
    fn names_and_presets_are_checked() {
        assert!(valid_name("Bot_1"));
        assert!(!valid_name(""));
        assert!(!valid_name("way_too_long_a_name"));
        assert!(!valid_name("has space"));
        assert!(skill_preset("EXPERT").is_some());
        assert!(skill_preset("god").is_none());
    }

    #[test]
    fn spawn_goto_stop_remove_round_trip() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        assert_eq!(
            rig.run(Cmd::Spawn("Bob".into()), &op),
            Ok("Spawned Bob".into())
        );
        assert!(rig.run(Cmd::Spawn("Bob".into()), &op).is_err());
        rig.run(Cmd::Goto("Bob".into(), vec![10, 64, 10]), &op)
            .unwrap();
        assert!(matches!(rig.process("Bob"), Process::Goto(_)));
        rig.run(Cmd::Stop("Bob".into()), &op).unwrap();
        assert_eq!(rig.process("Bob"), Process::Idle);
        rig.run(Cmd::Follow("Bob".into(), "bb".into()), &op)
            .unwrap();
        assert!(matches!(rig.process("Bob"), Process::Follow(_)));
        rig.run(Cmd::Fly("Bob".into(), 5, 6), &op).unwrap();
        assert!(matches!(rig.process("Bob"), Process::Fly(_)));
        rig.run(Cmd::Come("Bob".into()), &op).unwrap();
        assert!(matches!(rig.process("Bob"), Process::Goto(_)));
        rig.run(Cmd::Remove("Bob".into()), &op).unwrap();
        assert!(rig.run(Cmd::Stop("Bob".into()), &op).is_err());
        assert!(rig.bots.is_empty());
    }

    #[test]
    fn only_the_commander_or_an_admin_commands_a_bot() {
        let mut rig = Rig::new();
        rig.run(Cmd::Spawn("Bob".into()), &sender("aa", false))
            .unwrap();
        let other = sender("bb", false);
        assert_eq!(
            rig.run(Cmd::Stop("Bob".into()), &other),
            Err("Bob is not your bot".into())
        );
        assert!(rig.run(Cmd::Remove("Bob".into()), &other).is_err());
        assert!(
            rig.run(Cmd::Stop("Bob".into()), &sender("bb", true))
                .is_ok()
        );
    }

    #[test]
    fn list_shows_what_the_sender_may_command() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        assert_eq!(rig.run(Cmd::List, &op), Ok("No bots".into()));
        rig.run(Cmd::Spawn("Bob".into()), &op).unwrap();
        rig.run(Cmd::Spawn("Amy".into()), &sender("bb", false))
            .unwrap();
        let listed = rig.run(Cmd::List, &op).unwrap();
        assert!(listed.starts_with("Bob: idle"), "{listed}");
        assert!(!listed.contains("Amy"));
        assert!(
            rig.run(Cmd::List, &sender("cc", true))
                .unwrap()
                .contains("Amy")
        );
    }

    #[test]
    fn a_bot_lost_elsewhere_frees_its_name() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        rig.run(Cmd::Spawn("Bob".into()), &op).unwrap();
        let id = rig.reg.named["Bob"].id;
        rig.bots.remove(&mut rig.host, OWNER, id).unwrap();
        assert!(rig.run(Cmd::Stop("Bob".into()), &op).is_err());
        assert!(rig.run(Cmd::Spawn("Bob".into()), &op).is_ok());
    }

    #[test]
    fn bad_input_is_refused_before_touching_a_bot() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        assert!(rig.run(Cmd::Spawn("not ok".into()), &op).is_err());
        rig.run(Cmd::Spawn("Bob".into()), &op).unwrap();
        assert!(
            rig.run(Cmd::Goto("Bob".into(), vec![1, 2, 3, 4]), &op)
                .is_err()
        );
        assert!(
            rig.run(Cmd::Skill("Bob".into(), "god".into()), &op)
                .is_err()
        );
        assert!(
            rig.run(Cmd::Skill("Bob".into(), "expert".into()), &op)
                .is_ok()
        );
        assert_eq!(rig.bots.skill(OWNER, 1).unwrap().turn_rate, 60.0);
    }

    fn box_cmd(size: [i32; 3], block: &str, hollow: bool) -> Cmd {
        Cmd::Build(
            "Bob".into(),
            Shape::Box {
                size,
                block: block.into(),
                hollow,
            },
        )
    }

    #[test]
    fn build_box_sets_the_process_east_of_the_commander() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        rig.run(Cmd::Spawn("Bob".into()), &op).unwrap();
        rig.run(box_cmd([3, 3, 3], "cobblestone", true), &op)
            .unwrap();
        let Process::Build(spec) = rig.process("Bob") else {
            panic!("not building");
        };
        assert_eq!(
            spec.origin,
            BlockPos {
                x: 5 + BUILD_GAP,
                y: 64,
                z: 5
            }
        );
        assert_eq!(spec.blocks.len(), 26);
        assert_eq!(spec.blocks[0].state, "minecraft:cobblestone");
    }

    #[test]
    fn build_line_keeps_the_block_properties() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        rig.run(Cmd::Spawn("Bob".into()), &op).unwrap();
        rig.run(
            Cmd::Build(
                "Bob".into(),
                Shape::Line {
                    axis: "z".into(),
                    length: 2,
                    state: "oak_stairs[facing=east,half=top]".into(),
                },
            ),
            &op,
        )
        .unwrap();
        let Process::Build(spec) = rig.process("Bob") else {
            panic!("not building");
        };
        assert_eq!(spec.blocks.len(), 2);
        assert_eq!(
            spec.blocks[1].state,
            "minecraft:oak_stairs[facing=east,half=top]"
        );
    }

    #[test]
    fn build_rejects_bad_sizes_blocks_and_strangers() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        rig.run(Cmd::Spawn("Bob".into()), &op).unwrap();
        for bad in [[0, 3, 3], [20, 20, 20], [3, -1, 3]] {
            assert!(rig.run(box_cmd(bad, "stone", false), &op).is_err());
        }
        assert!(
            rig.run(box_cmd([3, 3, 3], "Not A Block", false), &op)
                .is_err()
        );
        assert!(rig.run(box_cmd([3, 3, 3], "stone[", false), &op).is_err());
        assert!(
            rig.run(box_cmd([3, 3, 3], "stone", false), &sender("bb", false))
                .is_err()
        );
        assert_eq!(rig.process("Bob"), Process::Idle);
    }

    #[test]
    fn file_paths_stay_inside_the_store() {
        assert_eq!(file_path(" houses/hut.txt ").unwrap(), "houses/hut.txt");
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../x",
            "a//b",
            "./a",
            "a\\b",
            "c:x",
            "a/",
        ] {
            assert!(file_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_file_build_starts_east_of_the_commander() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        rig.run(Cmd::Spawn("Bob".into()), &op).unwrap();
        let blocks = crate::build::parse_blocks("0 0 0 stone\n1 1 0 oak_log[axis=x]").unwrap();
        start_build(&mut rig.reg, &mut rig.bots, "Bob", &op, blocks).unwrap();
        let Process::Build(spec) = rig.process("Bob") else {
            panic!("not building");
        };
        assert_eq!(spec.origin.x, 5 + BUILD_GAP);
        assert_eq!(spec.blocks.len(), 2);
    }

    #[test]
    fn a_bad_file_or_stranger_leaves_the_process_alone() {
        let mut rig = Rig::new();
        let op = sender("aa", false);
        rig.run(Cmd::Spawn("Bob".into()), &op).unwrap();
        assert!(crate::build::parse_blocks("0 0 stone").is_err());
        let blocks = crate::build::parse_blocks("0 0 0 stone").unwrap();
        assert!(
            start_build(
                &mut rig.reg,
                &mut rig.bots,
                "Bob",
                &sender("bb", false),
                blocks
            )
            .is_err()
        );
        assert_eq!(rig.process("Bob"), Process::Idle);
    }
}
