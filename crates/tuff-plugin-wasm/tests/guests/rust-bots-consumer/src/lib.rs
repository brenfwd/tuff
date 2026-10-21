//! Test consumer of the bots plugin: `/botuser spawn <name>`, `remove`,
//! `owned`, `events` and `answer` drive the typed `control` interface and
//! report the `events` it receives.

use std::cell::RefCell;

use tuff_bots_api::{control, events};
use tuff_plugin_sdk::players;
use tuff_plugin_sdk::prelude::*;
use tuff_plugin_sdk::wire::Error;

thread_local! {
    static EVENTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static ANSWER: RefCell<String> = const { RefCell::new(String::new()) };
}

fn record(line: String) {
    EVENTS.with(|e| e.borrow_mut().push(line));
}

struct Sink;

impl events::Serve for Sink {
    fn spawned(&mut self, _from: &str, bot: u32) {
        record(format!("spawned {bot}"));
    }
    fn died(&mut self, _from: &str, bot: u32, cause: String) {
        record(format!("died {bot} {cause}"));
    }
    fn respawned(&mut self, _from: &str, bot: u32) {
        record(format!("respawned {bot}"));
    }
    fn process_done(&mut self, _from: &str, bot: u32) {
        record(format!("done {bot}"));
    }
    fn process_failed(&mut self, _from: &str, bot: u32, reason: String) {
        record(format!("failed {bot} {reason}"));
    }
    fn removed(&mut self, _from: &str, bot: u32) {
        record(format!("removed {bot}"));
    }
}

fn spec(name: String) -> control::SpawnSpec {
    spec_at(name, 0.5, 80.0, 0.5)
}

fn spec_at(name: String, x: f64, y: f64, z: f64) -> control::SpawnSpec {
    control::SpawnSpec {
        name,
        model: control::Model::Classic,
        location: control::Location {
            dimension: None,
            pos: control::Vec3 { x, y, z },
            yaw: None,
            pitch: None,
        },
        skill: control::Skill {
            reaction_ticks: 4,
            aim_error_degrees: 2.0,
            turn_rate: 30.0,
            click_cps_cap: 10.0,
            attack_charge: 0.9,
        },
        policy: control::Policy {
            allow_break: true,
            allow_place: true,
            allow_sprint: true,
            allow_parkour: true,
            max_fall: 3,
            break_allow: vec![],
            break_deny: vec![],
            throwaway_items: vec!["minecraft:cobblestone".into()],
            avoid: vec![],
            auto_eat: false,
        },
        respawn: control::RespawnPolicy::Owner,
    }
}

fn answer(ctx: &Ctx, line: String) {
    ANSWER.with(|a| a.replace(line.clone()));
    let _ = ctx.reply(line);
}

fn describe<E: std::fmt::Debug>(error: Error<E>) -> String {
    match error {
        Error::Call(CallError::NoProvider) => "bots is not installed".to_owned(),
        other => format!("failed: {other:?}"),
    }
}

fn int(ctx: &Ctx, name: &str) -> i32 {
    ctx.get::<String>(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

struct Consumer;

impl Plugin for Consumer {
    fn register() -> Result<(), String> {
        events::serve(Sink).map_err(|e| format!("{e:?}"))?;
        command("botuser")
            .then(
                literal("spawn").then(argument("name", parser::word()).executes_async(
                    |ctx| async move {
                        let name = ctx.get::<String>("name").unwrap_or_default();
                        let line = match control::Control::any().spawn(spec(name)).await {
                            Ok(id) => format!("spawned {id}"),
                            Err(error) => describe(error),
                        };
                        answer(&ctx, line);
                    },
                )),
            )
            .then(
                literal("remove").then(argument("id", parser::word()).executes_async(
                    |ctx| async move {
                        let id = ctx
                            .get::<String>("id")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        let line = match control::Control::any().remove(id).await {
                            Ok(()) => format!("removed {id}"),
                            Err(error) => describe(error),
                        };
                        answer(&ctx, line);
                    },
                )),
            )
            .then(
                literal("spawnat").then(argument("name", parser::word()).then(
                    argument("x", parser::word()).then(argument("y", parser::word()).then(
                        argument("z", parser::word()).executes_async(|ctx| async move {
                            let name = ctx.get::<String>("name").unwrap_or_default();
                            let at = |n: &str| f64::from(int(&ctx, n)) + 0.5;
                            let spec = spec_at(name, at("x"), at("y") - 0.5, at("z"));
                            let line = match control::Control::any().spawn(spec).await {
                                Ok(id) => format!("spawned {id}"),
                                Err(error) => describe(error),
                            };
                            answer(&ctx, line);
                        }),
                    )),
                )),
            )
            .then(literal("goto").then(argument("id", parser::word()).then(
                argument("x", parser::word()).then(argument("y", parser::word()).then(
                    argument("z", parser::word()).executes_async(|ctx| async move {
                        let id = int(&ctx, "id") as u32;
                        let goal =
                            control::Goal::Leaf(control::LeafGoal::Block(control::BlockPos {
                                x: int(&ctx, "x"),
                                y: int(&ctx, "y"),
                                z: int(&ctx, "z"),
                            }));
                        let line = match control::Control::any()
                            .set_process(id, control::Process::Goto(goal))
                            .await
                        {
                            Ok(()) => format!("going {id}"),
                            Err(error) => describe(error),
                        };
                        answer(&ctx, line);
                    }),
                )),
            )))
            .then(literal("follow").then(argument("id", parser::word()).then(
                argument("target", parser::word()).then(
                    argument("range", parser::word()).executes_async(|ctx| async move {
                        let id = int(&ctx, "id") as u32;
                        let name = ctx.get::<String>("target").unwrap_or_default();
                        let Some(target) = players::online().into_iter().find(|p| p.name == name)
                        else {
                            answer(&ctx, format!("{name} is not online"));
                            return;
                        };
                        let spec = control::FollowSpec {
                            target: target.uuid.to_string(),
                            range: f64::from(int(&ctx, "range")),
                        };
                        let line = match control::Control::any()
                            .set_process(id, control::Process::Follow(spec))
                            .await
                        {
                            Ok(()) => format!("following {id}"),
                            Err(error) => describe(error),
                        };
                        answer(&ctx, line);
                    }),
                ),
            )))
            .then(literal("mine").then(argument("id", parser::word()).then(
                argument("block", parser::word()).then(
                    argument("count", parser::word()).executes_async(|ctx| async move {
                        let id = int(&ctx, "id") as u32;
                        let spec = control::MineSpec {
                            blocks: vec![ctx.get::<String>("block").unwrap_or_default()],
                            count: int(&ctx, "count") as u32,
                            radius: 16,
                        };
                        let line = match control::Control::any()
                            .set_process(id, control::Process::Mine(spec))
                            .await
                        {
                            Ok(()) => format!("mining {id}"),
                            Err(error) => describe(error),
                        };
                        answer(&ctx, line);
                    }),
                ),
            )))
            .then(
                literal("attack").then(argument("id", parser::word()).then(
                    argument("target", parser::word()).executes_async(|ctx| async move {
                        let id = int(&ctx, "id") as u32;
                        let spec = control::AttackSpec {
                            target: ctx.get::<String>("target").unwrap_or_default(),
                            until: control::AttackUntil::Death,
                            retreat_health: 0.0,
                        };
                        let line = match control::Control::any()
                            .set_process(id, control::Process::Attack(spec))
                            .await
                        {
                            Ok(()) => format!("attacking {id}"),
                            Err(error) => describe(error),
                        };
                        answer(&ctx, line);
                    }),
                )),
            )
            .then(
                literal("shoot").then(argument("id", parser::word()).then(
                    argument("target", parser::word()).executes_async(|ctx| async move {
                        let id = int(&ctx, "id") as u32;
                        let spec = control::ShootSpec {
                            target: ctx.get::<String>("target").unwrap_or_default(),
                            until: control::AttackUntil::Death,
                            melee_range: 4.0,
                        };
                        let line = match control::Control::any()
                            .set_process(id, control::Process::Shoot(spec))
                            .await
                        {
                            Ok(()) => format!("shooting {id}"),
                            Err(error) => describe(error),
                        };
                        answer(&ctx, line);
                    }),
                )),
            )
            .then(
                literal("useitem").then(argument("id", parser::word()).then(
                    argument("item", parser::word()).then(
                        argument("target", parser::word()).executes_async(|ctx| async move {
                            let id = int(&ctx, "id") as u32;
                            let spec = control::UseItemSpec {
                                item: ctx.get::<String>("item").unwrap_or_default(),
                                look_at: Some(control::LookTarget::Entity(
                                    ctx.get::<String>("target").unwrap_or_default(),
                                )),
                                hold_ticks: 0,
                            };
                            let line = match control::Control::any()
                                .set_process(id, control::Process::UseItem(spec))
                                .await
                            {
                                Ok(()) => format!("using {id}"),
                                Err(error) => describe(error),
                            };
                            answer(&ctx, line);
                        }),
                    ),
                )),
            )
            .then(literal("fly").then(argument("id", parser::word()).then(
                argument("x", parser::word()).then(argument("z", parser::word()).executes_async(
                    |ctx| async move {
                        let id = int(&ctx, "id") as u32;
                        let spec = control::FlySpec {
                            x: int(&ctx, "x"),
                            z: int(&ctx, "z"),
                            fireworks: 16,
                        };
                        let line = match control::Control::any()
                            .set_process(id, control::Process::Fly(spec))
                            .await
                        {
                            Ok(()) => format!("flying {id}"),
                            Err(error) => describe(error),
                        };
                        answer(&ctx, line);
                    },
                )),
            )))
            .then(literal("owned").executes_async(|ctx| async move {
                let line = match control::Control::any().owned().await {
                    Ok(ids) => format!("owned {ids:?}"),
                    Err(error) => format!("failed: {error:?}"),
                };
                answer(&ctx, line);
            }))
            .then(literal("online").executes(|ctx| {
                let names: Vec<String> = players::online().into_iter().map(|p| p.name).collect();
                ctx.reply(format!("online {}", names.join(",")))?;
                Ok(1)
            }))
            .then(
                literal("state").then(argument("id", parser::word()).executes_async(
                    |ctx| async move {
                        let id = ctx
                            .get::<String>("id")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        let line = match control::Control::any().state(id).await {
                            Ok(s) => format!(
                                "state {id} alive={} x={} y={} z={}",
                                s.alive, s.position.x, s.position.y, s.position.z
                            ),
                            Err(error) => describe(error),
                        };
                        answer(&ctx, line);
                    },
                )),
            )
            .then(literal("events").executes(|ctx| {
                ctx.reply(EVENTS.with(|e| e.borrow().join(",")))?;
                Ok(1)
            }))
            .then(literal("answer").executes(|ctx| {
                ctx.reply(ANSWER.with(|a| a.borrow().clone()))?;
                Ok(1)
            }))
            .register()?;
        Ok(())
    }
}

plugin!(Consumer {
    id: "bots-consumer",
    version: "0.1.0",
    description: "Test consumer of the bots plugin",
    soft_depends: "bots",
    requests: ["cap.cross-plugin = all", "cap.player-read = all"],
});
