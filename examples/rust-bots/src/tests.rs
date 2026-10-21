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

    fn drive(&mut self, _: Body, _: crate::exec::Drive) {}

    fn blocks(&mut self, _: Body) -> Option<&mut dyn crate::world::Blocks> {
        None
    }

    fn locate(&mut self, _: Body, _: &str) -> Option<Vec3> {
        None
    }

    fn inventory(&mut self, _: Body) -> Vec<crate::hands::Carried> {
        self.carried.clone()
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
