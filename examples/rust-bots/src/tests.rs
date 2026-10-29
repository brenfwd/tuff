// SPDX-License-Identifier: MIT
use std::collections::BTreeMap;

use tuff_bots_api::control::*;

use crate::core::*;

#[derive(Default)]
pub(crate) struct Recording {
    next: Body,
    bodies: BTreeMap<Body, Perceived>,
    pub(crate) loaded: Vec<String>,
    events: Vec<(String, BotId, Event)>,
    removed: Vec<Body>,
    respawned: Vec<Body>,
    gone_owners: Vec<String>,
    pub(crate) carried: Vec<crate::hands::Carried>,
    pub(crate) uses: Vec<&'static str>,
    lookups: std::collections::VecDeque<Lookup>,
    log: Vec<String>,
    yaws: Vec<f32>,
    nearby: Vec<crate::danger::Nearby>,
    nearby_calls: u32,
    ground: Option<Ground>,
}

pub(crate) struct Ground {
    ore: Option<(i32, i32, i32)>,
}

impl crate::world::Blocks for Ground {
    fn class_at(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
    ) -> Result<crate::world::Class, crate::world::Pending> {
        use crate::world::Class;
        let ore = self.ore == Some((x, y, z));
        Ok(if y < 64 || ore {
            Class::STANDABLE
        } else {
            Class::PASSABLE
        })
    }

    fn state_at(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
    ) -> Result<Option<std::rc::Rc<str>>, crate::world::Pending> {
        Ok((self.ore == Some((x, y, z))).then(|| "minecraft:iron_ore".into()))
    }

    fn find(
        &mut self,
        _section: crate::world::SectionKey,
        _wanted: &[String],
    ) -> Result<Vec<(i32, i32, i32)>, crate::world::Pending> {
        Ok(self.ore.into_iter().collect())
    }
}

impl Host for Recording {
    fn spawn(&mut self, spec: &SpawnSpec) -> Result<Body, ControlError> {
        if spec.name.is_empty() {
            return Err(ControlError::Invalid("name".into()));
        }
        self.next += 1;
        self.bodies
            .insert(self.next, Perceived::standing(spec.location.pos.clone()));
        Ok(self.next)
    }

    fn remove(&mut self, body: Body) {
        self.bodies.remove(&body);
        self.removed.push(body);
    }

    fn perceive(&mut self, body: Body) -> Option<Perceived> {
        self.bodies.get(&body).cloned()
    }

    fn respawn(&mut self, body: Body) {
        self.respawned.push(body);
        if let Some(b) = self.bodies.get_mut(&body) {
            b.alive = true;
            b.health = 20.0;
        }
    }

    fn drive(&mut self, _: Body, drive: crate::exec::Drive) {
        self.yaws.push(drive.yaw);
    }

    fn blocks(&mut self, _: Body) -> Option<&mut dyn crate::world::Blocks> {
        self.ground
            .as_mut()
            .map(|g| g as &mut dyn crate::world::Blocks)
    }

    fn select_slot(&mut self, _: Body, slot: u8) {
        self.log.push(format!("slot {slot}"));
    }

    fn dig(&mut self, _: Body, pos: (i32, i32, i32), _: &Vec3) {
        self.log.push(format!("dig {pos:?}"));
    }

    fn nearby(&mut self, _: Body, _: f64) -> Vec<crate::danger::Nearby> {
        self.nearby_calls += 1;
        self.nearby.clone()
    }

    fn locate(&mut self, _: Body, _: &str) -> Option<Vec3> {
        None
    }

    fn inventory(&mut self, _: Body) -> Vec<crate::hands::Carried> {
        self.carried.clone()
    }

    fn target(&mut self, _: Body, _: &str) -> Lookup {
        self.lookups.pop_front().unwrap_or(Lookup::Gone)
    }

    fn use_held(&mut self, _: Body) {
        self.uses.push("use");
    }

    fn stop_using(&mut self, _: Body) {
        self.uses.push("stop");
    }

    fn owner_loaded(&self, owner: &str) -> bool {
        self.loaded.iter().any(|o| o == owner)
    }

    fn notify(&mut self, owner: &str, bot: BotId, event: Event) -> Delivery {
        if self.gone_owners.iter().any(|o| o == owner) {
            return Delivery::Gone;
        }
        self.events.push((owner.to_owned(), bot, event));
        Delivery::Sent
    }
}

fn spec(name: &str, respawn: RespawnPolicy) -> SpawnSpec {
    SpawnSpec {
        name: name.into(),
        model: Model::Classic,
        location: Location {
            dimension: None,
            pos: Vec3 {
                x: 0.5,
                y: 64.0,
                z: 0.5,
            },
            yaw: None,
            pitch: None,
        },
        skill: Skill {
            reaction_ticks: 4,
            aim_error_degrees: 2.0,
            turn_rate: 30.0,
            click_cps_cap: 10.0,
            attack_charge: 0.9,
        },
        policy: Policy {
            allow_break: true,
            allow_place: true,
            allow_sprint: true,
            allow_parkour: false,
            max_fall: 3,
            break_allow: vec![],
            break_deny: vec![],
            throwaway_items: vec![],
            avoid: vec![],
            auto_eat: false,
            defend: false,
        },
        respawn,
    }
}

fn host_with(owners: &[&str]) -> Recording {
    Recording {
        loaded: owners.iter().map(|o| (*o).to_owned()).collect(),
        ..Recording::default()
    }
}

fn events_of(host: &Recording, bot: BotId) -> Vec<Event> {
    host.events
        .iter()
        .filter(|(_, b, _)| *b == bot)
        .map(|(_, _, e)| e.clone())
        .collect()
}

#[test]
fn only_the_owner_controls_a_bot() {
    let mut host = host_with(&["a", "b"]);
    let mut bots = Bots::default();
    let id = bots
        .spawn(&mut host, "a", spec("One", RespawnPolicy::Owner))
        .unwrap();
    assert_eq!(bots.list("a"), vec![id]);
    assert!(bots.list("b").is_empty());
    assert_eq!(
        bots.set_process("b", id, Process::Idle),
        Err(ControlError::NotOwner)
    );
    assert_eq!(bots.state("b", id), Err(ControlError::NotOwner));
    assert_eq!(bots.remove(&mut host, "b", id), Err(ControlError::NotOwner));
    assert_eq!(bots.stop("a", 99), Err(ControlError::UnknownBot));
    assert!(bots.state("a", id).unwrap().alive);
    assert_eq!(events_of(&host, id), vec![Event::Spawned]);
}

#[test]
fn spawn_errors_come_from_the_host_and_the_owner_limit() {
    let mut host = host_with(&["a", "b"]);
    let mut bots = Bots::default();
    assert_eq!(
        bots.spawn(&mut host, "a", spec("", RespawnPolicy::Owner)),
        Err(ControlError::Invalid("name".into()))
    );
    for i in 0..MAX_BOTS_PER_OWNER {
        bots.spawn(&mut host, "a", spec(&format!("B{i}"), RespawnPolicy::Owner))
            .unwrap();
    }
    assert_eq!(
        bots.spawn(&mut host, "a", spec("Over", RespawnPolicy::Owner)),
        Err(ControlError::LimitReached)
    );
    assert!(
        bots.spawn(&mut host, "b", spec("Other", RespawnPolicy::Owner))
            .is_ok()
    );
}

#[test]
fn remove_deletes_the_body_and_reports_removed() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let id = bots
        .spawn(&mut host, "a", spec("One", RespawnPolicy::Owner))
        .unwrap();
    bots.remove(&mut host, "a", id).unwrap();
    assert!(bots.is_empty());
    assert_eq!(host.removed.len(), 1);
    assert_eq!(events_of(&host, id), vec![Event::Spawned, Event::Removed]);
    assert_eq!(
        bots.remove(&mut host, "a", id),
        Err(ControlError::UnknownBot)
    );
}

#[test]
fn set_process_replaces_and_goto_reports_done_once() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let id = bots
        .spawn(&mut host, "a", spec("One", RespawnPolicy::Owner))
        .unwrap();
    let far = Goal::Leaf(LeafGoal::Block(BlockPos { x: 50, y: 64, z: 0 }));
    let here = Goal::Leaf(LeafGoal::Xz(XzGoal { x: 0, z: 0 }));
    bots.set_process("a", id, Process::Goto(far.clone()))
        .unwrap();
    bots.tick(&mut host);
    assert_eq!(bots.state("a", id).unwrap().process, Process::Goto(far));
    bots.set_process("a", id, Process::Goto(here)).unwrap();
    bots.tick(&mut host);
    bots.tick(&mut host);
    assert_eq!(bots.state("a", id).unwrap().process, Process::Idle);
    assert_eq!(
        events_of(&host, id),
        vec![Event::Spawned, Event::ProcessDone]
    );
}

#[test]
fn stop_goes_idle_without_an_event() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let id = bots
        .spawn(&mut host, "a", spec("One", RespawnPolicy::Owner))
        .unwrap();
    let far = Goal::Leaf(LeafGoal::Block(BlockPos { x: 50, y: 64, z: 0 }));
    bots.set_process("a", id, Process::Goto(far)).unwrap();
    bots.stop("a", id).unwrap();
    bots.tick(&mut host);
    assert_eq!(bots.state("a", id).unwrap().process, Process::Idle);
    assert_eq!(events_of(&host, id), vec![Event::Spawned]);
}

#[test]
fn skill_and_policy_are_stored_per_bot() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let id = bots
        .spawn(&mut host, "a", spec("One", RespawnPolicy::Owner))
        .unwrap();
    let mut skill = bots.skill("a", id).unwrap();
    skill.reaction_ticks = 9;
    bots.set_skill("a", id, skill).unwrap();
    assert_eq!(bots.skill("a", id).unwrap().reaction_ticks, 9);
    let mut policy = bots.policy("a", id).unwrap();
    policy.allow_break = false;
    bots.set_policy("a", id, policy).unwrap();
    assert!(!bots.policy("a", id).unwrap().allow_break);
}

#[test]
fn death_is_reported_once_and_auto_respawn_follows() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let id = bots
        .spawn(&mut host, "a", spec("One", RespawnPolicy::Auto(3)))
        .unwrap();
    host.bodies.get_mut(&1).unwrap().alive = false;
    bots.tick(&mut host);
    bots.tick(&mut host);
    assert!(host.respawned.is_empty());
    bots.tick(&mut host);
    assert_eq!(host.respawned, vec![1]);
    bots.tick(&mut host);
    assert_eq!(
        events_of(&host, id),
        vec![
            Event::Spawned,
            Event::Died("unknown".into()),
            Event::Respawned
        ]
    );
}

#[test]
fn owner_driven_respawn_waits_for_the_owner() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let id = bots
        .spawn(&mut host, "a", spec("One", RespawnPolicy::Owner))
        .unwrap();
    assert!(bots.respawn(&mut host, "a", id).is_err());
    host.bodies.get_mut(&1).unwrap().alive = false;
    for _ in 0..50 {
        bots.tick(&mut host);
    }
    assert!(host.respawned.is_empty());
    bots.respawn(&mut host, "b_not_owner", id).unwrap_err();
    bots.respawn(&mut host, "a", id).unwrap();
    assert_eq!(host.respawned, vec![1]);
}

#[test]
fn an_unloaded_owner_loses_its_bots_within_a_second() {
    let mut host = host_with(&["a", "b"]);
    let mut bots = Bots::default();
    bots.spawn(&mut host, "a", spec("One", RespawnPolicy::Owner))
        .unwrap();
    bots.spawn(&mut host, "b", spec("Two", RespawnPolicy::Owner))
        .unwrap();
    host.loaded.retain(|o| o != "a");
    for _ in 0..19 {
        bots.tick(&mut host);
    }
    assert_eq!(bots.len(), 2);
    bots.tick(&mut host);
    assert_eq!(bots.len(), 1);
    assert_eq!(bots.list("b").len(), 1);
    assert_eq!(host.removed, vec![1]);
}

#[test]
fn a_gone_notify_checks_the_owner_immediately() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    bots.spawn(&mut host, "a", spec("One", RespawnPolicy::Owner))
        .unwrap();
    host.gone_owners.push("a".into());
    bots.spawn(&mut host, "a", spec("Two", RespawnPolicy::Owner))
        .unwrap();
    assert_eq!(bots.len(), 2, "owner is still loaded, so its bots stay");
    host.loaded.clear();
    bots.spawn(&mut host, "a", spec("Three", RespawnPolicy::Owner))
        .unwrap();
    assert!(bots.list("a").is_empty());
    assert_eq!(host.removed.len(), 3);
}

#[test]
fn a_vanished_body_removes_the_bot() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let id = bots
        .spawn(&mut host, "a", spec("One", RespawnPolicy::Owner))
        .unwrap();
    host.bodies.clear();
    bots.tick(&mut host);
    assert!(bots.is_empty());
    assert_eq!(events_of(&host, id), vec![Event::Spawned, Event::Removed]);
}

#[test]
fn goals_are_reached_by_their_own_rules() {
    let at = |x, y, z| Vec3 { x, y, z };
    let block = |x, y, z| BlockPos { x, y, z };
    assert!(goal_reached(
        &Goal::Leaf(LeafGoal::Block(block(1, 64, 2))),
        &at(1.9, 64.0, 2.1)
    ));
    assert!(!goal_reached(
        &Goal::Leaf(LeafGoal::Block(block(1, 64, 2))),
        &at(2.1, 64.0, 2.1)
    ));
    let near = LeafGoal::Near(NearGoal {
        pos: block(0, 64, 0),
        radius: 3.0,
    });
    assert!(goal_reached(&Goal::Leaf(near.clone()), &at(2.0, 64.0, 0.5)));
    assert!(!goal_reached(&Goal::Leaf(near), &at(5.0, 64.0, 0.5)));
    assert!(goal_reached(
        &Goal::Leaf(LeafGoal::YLevel(70)),
        &at(9.0, 70.5, 9.0)
    ));
    let adjacent = Goal::Leaf(LeafGoal::GetToBlock(block(5, 64, 5)));
    assert!(goal_reached(&adjacent, &at(4.5, 64.0, 5.5)));
    assert!(!goal_reached(&adjacent, &at(5.5, 64.0, 5.5)));
    assert!(!goal_reached(&adjacent, &at(4.5, 64.0, 4.5)));
    let away = Goal::Leaf(LeafGoal::RunAway(RunAwayGoal {
        origin: block(0, 64, 0),
        distance: 10.0,
    }));
    assert!(!goal_reached(&away, &at(3.0, 64.0, 3.0)));
    assert!(goal_reached(&away, &at(12.0, 64.0, 0.5)));
    let any = Goal::AnyOf(vec![
        LeafGoal::YLevel(100),
        LeafGoal::Xz(XzGoal { x: 0, z: 0 }),
    ]);
    assert!(goal_reached(&any, &at(0.5, 64.0, 0.5)));
    assert!(!goal_reached(&Goal::AnyOf(vec![]), &at(0.5, 64.0, 0.5)));
}

#[test]
fn a_hungry_bot_with_auto_eat_eats_bread_once() {
    let mut host = host_with(&["a"]);
    host.carried = vec![(40, "minecraft:bread".into(), 2)];
    let mut sp = spec("Eater", RespawnPolicy::Owner);
    sp.policy.auto_eat = true;
    let mut bots = Bots::default();
    bots.spawn(&mut host, "a", sp).unwrap();
    for b in host.bodies.values_mut() {
        b.food = 10;
    }
    for _ in 0..36 {
        bots.tick(&mut host);
    }
    assert_eq!(host.uses, vec!["use", "stop"]);
}

#[test]
fn a_full_bot_does_not_eat() {
    let mut host = host_with(&["a"]);
    host.carried = vec![(40, "minecraft:bread".into(), 2)];
    let mut sp = spec("Full", RespawnPolicy::Owner);
    sp.policy.auto_eat = true;
    let mut bots = Bots::default();
    bots.spawn(&mut host, "a", sp).unwrap();
    for _ in 0..60 {
        bots.tick(&mut host);
    }
    assert!(host.uses.is_empty());
}

fn alive_target() -> Lookup {
    Lookup::Found(crate::combat::Target {
        pos: Vec3 {
            x: 1.5,
            y: 64.0,
            z: 0.5,
        },
        alive: true,
        width: 0.6,
        height: 1.95,
    })
}

#[test]
fn an_unknown_target_lookup_keeps_the_fight_going_and_gone_ends_it() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let id = bots
        .spawn(&mut host, "a", spec("One", RespawnPolicy::Owner))
        .unwrap();
    let attack = Process::Attack(AttackSpec {
        target: "zombie".into(),
        until: AttackUntil::Death,
        retreat_health: 0.0,
    });
    bots.set_process("a", id, attack.clone()).unwrap();
    host.lookups = [
        alive_target(),
        Lookup::Unknown,
        Lookup::Unknown,
        alive_target(),
    ]
    .into();
    for _ in 0..4 {
        bots.tick(&mut host);
        assert_eq!(bots.state("a", id).unwrap().process, attack);
    }
    bots.tick(&mut host);
    assert_eq!(bots.state("a", id).unwrap().process, Process::Idle);
    assert_eq!(
        events_of(&host, id),
        vec![Event::Spawned, Event::ProcessDone]
    );
}

fn nearby(kind: &str, x: f64, z: f64) -> crate::danger::Nearby {
    crate::danger::Nearby {
        kind: kind.into(),
        pos: Vec3 { x, y: 64.0, z },
        velocity: Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
    }
}

fn goto_north(host: &mut Recording, bots: &mut Bots, auto_eat: bool) -> BotId {
    let mut sp = spec("Walker", RespawnPolicy::Owner);
    sp.policy.auto_eat = auto_eat;
    let id = bots.spawn(host, "a", sp).unwrap();
    host.ground = Some(Ground { ore: None });
    let goal = Goal::Leaf(LeafGoal::Xz(XzGoal { x: 0, z: 30 }));
    bots.set_process("a", id, Process::Goto(goal)).unwrap();
    id
}

#[test]
fn a_miner_swaps_to_the_best_pickaxe_before_it_digs() {
    let mut host = host_with(&["a"]);
    host.carried = vec![
        (36, "minecraft:wooden_pickaxe".into(), 1),
        (38, "minecraft:iron_pickaxe".into(), 1),
    ];
    host.ground = Some(Ground {
        ore: Some((1, 64, 0)),
    });
    let mut bots = Bots::default();
    let id = bots
        .spawn(&mut host, "a", spec("Miner", RespawnPolicy::Owner))
        .unwrap();
    let mine = Process::Mine(MineSpec {
        blocks: vec!["iron_ore".into()],
        count: 1,
        radius: 6,
    });
    bots.set_process("a", id, mine).unwrap();
    for _ in 0..10 {
        bots.tick(&mut host);
    }
    let first_dig = host.log.iter().position(|l| l.starts_with("dig"));
    let first_dig = first_dig.expect("the bot dug");
    assert!(first_dig > 0, "{:?}", host.log);
    assert_eq!(host.log[first_dig - 1], "slot 2", "{:?}", host.log);
}

#[test]
fn nearby_entities_are_polled_every_few_ticks_and_not_while_idle() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let id = goto_north(&mut host, &mut bots, false);
    for _ in 0..20 {
        bots.tick(&mut host);
    }
    assert_eq!(host.nearby_calls, 4);
    bots.stop("a", id).unwrap();
    let calls = host.nearby_calls;
    for _ in 0..20 {
        bots.tick(&mut host);
    }
    assert_eq!(host.nearby_calls, calls);
}

fn farthest_turn(host: &Recording) -> f32 {
    host.yaws.iter().fold(0.0_f32, |m, y| m.max(y.abs()))
}

#[test]
fn a_zombie_ahead_turns_the_path_aside() {
    let mut clear = host_with(&["a"]);
    let mut bots = Bots::default();
    goto_north(&mut clear, &mut bots, false);
    for _ in 0..10 {
        bots.tick(&mut clear);
    }
    let mut host = host_with(&["a"]);
    host.nearby = vec![nearby("minecraft:zombie", 0.5, 6.5)];
    let mut bots = Bots::default();
    goto_north(&mut host, &mut bots, false);
    for _ in 0..10 {
        bots.tick(&mut host);
    }
    assert!(farthest_turn(&clear) < 5.0, "{:?}", clear.yaws);
    assert!(farthest_turn(&host) > 10.0, "{:?}", host.yaws);
}

#[test]
fn a_drop_beside_the_route_is_a_detour_only_with_upkeep_on() {
    let drop = nearby("minecraft:item", 3.5, 2.5);
    let mut off = host_with(&["a"]);
    off.nearby = vec![drop.clone()];
    let mut bots = Bots::default();
    goto_north(&mut off, &mut bots, false);
    for _ in 0..10 {
        bots.tick(&mut off);
    }
    let mut on = host_with(&["a"]);
    on.nearby = vec![drop];
    let mut bots = Bots::default();
    goto_north(&mut on, &mut bots, true);
    for _ in 0..10 {
        bots.tick(&mut on);
    }
    assert!(farthest_turn(&off) < 5.0, "{:?}", off.yaws);
    assert!(farthest_turn(&on) > 20.0, "{:?}", on.yaws);
}

#[test]
fn a_better_armor_piece_in_the_hotbar_is_put_on_once() {
    let mut host = host_with(&["a"]);
    host.carried = vec![(37, "minecraft:iron_helmet".into(), 1)];
    let mut sp = spec("Dresser", RespawnPolicy::Owner);
    sp.policy.auto_eat = true;
    let mut bots = Bots::default();
    bots.spawn(&mut host, "a", sp).unwrap();
    bots.tick(&mut host);
    assert_eq!(host.log, vec!["slot 1".to_string()]);
    assert_eq!(host.uses, vec!["use"]);
}

fn defender(host: &mut Recording, bots: &mut Bots, defend: bool) -> (BotId, Process) {
    let mut sp = spec("One", RespawnPolicy::Owner);
    sp.policy.defend = defend;
    let id = bots.spawn(host, "a", sp).unwrap();
    let goto = Process::Goto(Goal::Leaf(LeafGoal::Xz(XzGoal { x: 0, z: -40 })));
    bots.set_process("a", id, goto.clone()).unwrap();
    (id, goto)
}

#[test]
fn defence_a_hurt_bot_fights_then_resumes_its_process_silently() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let (id, goto) = defender(&mut host, &mut bots, true);
    let attacker = "ab".repeat(16);
    bots.hurt(host.next, attacker.clone());
    host.lookups = [alive_target(), alive_target(), Lookup::Gone].into();
    bots.tick(&mut host);
    match bots.state("a", id).unwrap().process {
        Process::Attack(spec) => assert_eq!(spec.target, attacker),
        other => panic!("expected a fight, got {other:?}"),
    }
    bots.tick(&mut host);
    assert_eq!(bots.state("a", id).unwrap().process, goto);
    assert_eq!(events_of(&host, id), vec![Event::Spawned]);
}

#[test]
fn defence_a_bot_without_the_flag_ignores_hits() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let (id, goto) = defender(&mut host, &mut bots, false);
    bots.hurt(host.next, "ab".repeat(16));
    host.lookups = [alive_target(), alive_target()].into();
    bots.tick(&mut host);
    assert_eq!(bots.state("a", id).unwrap().process, goto);
}

#[test]
fn defence_a_forgotten_hit_ends_the_fight() {
    let mut host = host_with(&["a"]);
    let mut bots = Bots::default();
    let (id, goto) = defender(&mut host, &mut bots, true);
    bots.hurt(host.next, "ab".repeat(16));
    host.lookups = std::iter::repeat_with(alive_target).take(400).collect();
    bots.tick(&mut host);
    assert!(matches!(
        bots.state("a", id).unwrap().process,
        Process::Attack(_)
    ));
    for _ in 0..=crate::defence::MEMORY_TICKS {
        bots.tick(&mut host);
    }
    assert_eq!(bots.state("a", id).unwrap().process, goto);
}
