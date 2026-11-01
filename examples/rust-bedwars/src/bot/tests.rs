// SPDX-License-Identifier: MIT
use super::goals::{Flags, Goal, GoalSelector};
use super::*;

fn view<'a>(me: [f64; 3], enemies: &'a [Seen]) -> View<'a> {
    View {
        role: Role::Solo,
        ally: None,
        now: 0,
        elapsed: 0,
        alive_for: u64::MAX,
        me,
        edge: None,
        home: me,
        forge: me,
        saving: false,
        bed: None,
        enemy_beds: &[],
        enemies,
        skill: Difficulty::Normal.skill(),
        bow: false,
        arrows: 0,
        fireballs: 0,
    }
}

#[derive(Debug)]
struct Probe {
    name: &'static str,
    flags: Flags,
    usable: bool,
    interruptable: bool,
}

impl Goal for Probe {
    fn name(&self) -> &'static str {
        self.name
    }
    fn flags(&self) -> Flags {
        self.flags
    }
    fn can_use(&mut self, view: &View) -> bool {
        self.usable && view.enemies.len() < 10
    }
    fn interruptable(&self) -> bool {
        self.interruptable
    }
    fn tick(&mut self, _view: &View, out: &mut Intent) {
        if self.flags.overlaps(Flags::MOVE) {
            out.goto = Some([0.0; 3]);
        }
        if self.flags.overlaps(Flags::TARGET) {
            out.fight = Some(0);
        }
    }
}

fn probe(name: &'static str, flags: Flags) -> Probe {
    Probe {
        name,
        flags,
        usable: true,
        interruptable: true,
    }
}

#[test]
fn goals_with_free_controls_run_together_and_higher_priorities_win() {
    let v = view([0.0; 3], &[]);
    let mut brain = GoalSelector::default();
    brain.add(3, probe("wander", Flags::MOVE));
    brain.add(5, probe("stare", Flags::TARGET));
    brain.tick(&v);
    assert_eq!(brain.running(), ["wander", "stare"]);
    let mut brain = GoalSelector::default();
    brain.add(3, probe("wander", Flags::MOVE));
    brain.add(1, probe("chase", Flags::MOVE.with(Flags::TARGET)));
    brain.add(5, probe("stare", Flags::TARGET));
    brain.tick(&v);
    assert_eq!(brain.running(), ["chase"], "chase holds both controls");
}

#[test]
fn a_higher_priority_goal_interrupts_only_interruptable_ones() {
    let v = view([0.0; 3], &[]);
    let mut brain = GoalSelector::default();
    brain.add(4, probe("wander", Flags::MOVE));
    brain.tick(&v);
    brain.add(1, probe("flee", Flags::MOVE));
    brain.tick(&v);
    assert_eq!(brain.running(), ["flee"]);

    let mut brain = GoalSelector::default();
    brain.add(
        4,
        Probe {
            interruptable: false,
            ..probe("busy", Flags::MOVE)
        },
    );
    brain.tick(&v);
    brain.add(1, probe("flee", Flags::MOVE));
    brain.tick(&v);
    assert_eq!(brain.running(), ["busy"]);
}

#[test]
fn a_goal_that_cannot_continue_stops_and_frees_its_controls() {
    let mut brain = GoalSelector::default();
    brain.add(1, probe("chase", Flags::MOVE));
    brain.add(2, probe("wander", Flags::MOVE));
    brain.tick(&view([0.0; 3], &[]));
    assert_eq!(brain.running(), ["chase"]);
    let crowd: Vec<Seen> = (0..10).map(|id| Seen { id, pos: [0.0; 3] }).collect();
    let intent = brain.tick(&view([0.0; 3], &crowd));
    assert!(brain.running().is_empty());
    assert_eq!(intent, Intent::default());
}

#[test]
fn bots_notice_enemies_only_after_their_reaction_time() {
    let mut bot = Bot::new(0, Difficulty::Normal, 1);
    let delay = bot.skill().reaction;
    let at = |t: u64| Seen {
        id: 7,
        pos: [t as f64, 64.0, 0.0],
    };
    for t in 0..delay {
        assert!(bot.perceive(t, &[at(t)]).is_empty(), "tick {t}");
    }
    for t in delay..delay + 20 {
        let seen = bot.perceive(t, &[at(t)]);
        assert_eq!(seen, [at(t - delay)], "tick {t}");
    }
    assert!(bot.perceive(100, &[]).is_empty());
    assert!(
        bot.perceive(101, &[at(101)]).is_empty(),
        "forgotten when gone"
    );
}

#[test]
fn harder_bots_react_faster_and_aim_better_but_none_exceed_a_player() {
    let [easy, normal, hard] = Difficulty::ALL.map(Difficulty::skill);
    assert!(easy.reaction > normal.reaction && normal.reaction > hard.reaction);
    assert!(easy.aim_error > normal.aim_error && normal.aim_error > hard.aim_error);
    assert!(hard.reaction >= 4, "no superhuman reactions");
    for d in Difficulty::ALL {
        assert_eq!(Difficulty::parse(d.name()), Some(d));
    }
}

fn archer_view<'a>(enemies: &'a [Seen], now: u64) -> View<'a> {
    View {
        now,
        bow: true,
        arrows: 5,
        fireballs: 0,
        ..view([0.0, 64.0, 0.0], enemies)
    }
}

#[test]
fn a_bow_needs_the_full_draw() {
    let mut bot = Bot::new(0, Difficulty::Hard, 1);
    let mut shot_at = None;
    for t in 0..40 {
        let enemies = [Seen {
            id: 1,
            pos: [20.0, 64.0, 0.4 * t as f64],
        }];
        let view = archer_view(&enemies, t);
        let intent = bot.think(&view);
        assert!(bot.goal_names().contains(&"shoot"));
        if intent.shoot == Some(Shot::Arrow) {
            shot_at = Some(t);
            break;
        }
    }
    assert_eq!(shot_at, Some(goals::DRAW_TICKS as u64 - 1));
}

#[test]
fn a_bot_throws_fire_charges_at_mid_range_and_melees_when_close() {
    let mut bot = Bot::new(0, Difficulty::Hard, 1);
    let far = [Seen {
        id: 1,
        pos: [8.0, 64.0, 0.0],
    }];
    let mut view = archer_view(&far, 0);
    view.bow = false;
    view.fireballs = 2;
    assert_eq!(bot.think(&view).shoot, Some(Shot::Fireball));
    view.now = 5;
    assert_eq!(bot.think(&view).shoot, None);
    view.now = 45;
    assert_eq!(bot.think(&view).shoot, Some(Shot::Fireball));
    let near = [Seen {
        id: 1,
        pos: [2.0, 64.0, 0.0],
    }];
    let view = View {
        enemies: &near,
        ..view
    };
    bot.think(&view);
    assert!(bot.goal_names().contains(&"fight"));
}

#[test]
fn bot_names_carry_the_prefix_and_fit_a_player_name() {
    assert_eq!(name(0), "BOT_Ashwin");
    assert_eq!(name(16), "BOT_Ashwin1");
    assert!(name(15 + 16 * 9).len() <= 16);
    assert_eq!(model(1), control::Model::Slim);
    let skill = Difficulty::Hard.skill().api();
    assert_eq!(skill.reaction_ticks, 4);
}

#[test]
fn roles_split_a_squad_into_defender_rusher_and_followers() {
    assert_eq!(Role::assign(0, 1), Role::Solo);
    assert_eq!(Role::assign(0, 3), Role::Defender);
    assert_eq!(Role::assign(1, 3), Role::Rusher);
    assert_eq!(Role::assign(2, 3), Role::Follower);
}

#[test]
fn defenders_never_rush_and_rushers_rush_early() {
    let beds = [[10, 64, 10]];
    let mut v = view([0.0, 64.0, 0.0], &[]);
    v.enemy_beds = &beds;
    v.elapsed = v.skill.rush_after / 3;
    let mut brain = goals::bedwars();
    v.role = Role::Defender;
    let out = brain.tick(&v);
    assert!(out.rush.is_none() && out.dig.is_none());
    let mut brain = goals::bedwars();
    v.role = Role::Rusher;
    let out = brain.tick(&v);
    assert_eq!(out.rush, Some(beds[0]));
    assert_eq!(brain.tick(&v).rush, Some(beds[0]));
    v.role = Role::Solo;
    let mut brain = goals::bedwars();
    v.home = v.me;
    assert!(brain.tick(&v).goto.is_none());
}

#[test]
fn followers_trail_a_distant_teammate() {
    let mut v = view([0.0, 64.0, 0.0], &[]);
    v.role = Role::Follower;
    v.ally = Some([20.0, 64.0, 0.0]);
    assert_eq!(goals::bedwars().tick(&v).goto, v.ally);
    v.ally = Some([2.0, 64.0, 0.0]);
    assert_eq!(goals::bedwars().tick(&v).goto, None);
}

#[test]
fn a_fighting_bot_stops_sprinting_in_melee_range() {
    let near = [Seen { id: 1, pos: [2.0, 0.0, 0.0] }];
    let far = [Seen { id: 1, pos: [8.0, 0.0, 0.0] }];
    let mut fight = goals::Fight::default();
    for (enemies, sprints) in [(&near, false), (&far, true)] {
        let v = view([0.0; 3], enemies);
        fight.start(&v);
        let mut out = Intent::default();
        fight.tick(&v, &mut out);
        assert_eq!(out.sprint, sprints);
    }
}

#[test]
fn a_respawned_bot_regroups_before_it_rushes_again() {
    let beds = [[10, 64, 10]];
    let mut v = view([0.0, 64.0, 0.0], &[]);
    v.enemy_beds = &beds;
    v.elapsed = v.skill.rush_after * 2;
    v.alive_for = 0;
    assert!(goals::bedwars().tick(&v).rush.is_none());
    v.alive_for = v.skill.rush_after / 3;
    assert_eq!(goals::bedwars().tick(&v).rush, Some(beds[0]));
}

#[test]
fn a_fighting_bot_at_an_edge_retreats_instead_of_chasing() {
    let enemies = [Seen { id: 1, pos: [2.0, 0.0, 0.0] }];
    let mut v = view([0.0; 3], &enemies);
    v.edge = Some([-3.5, 0.0, 0.5]);
    let mut fight = goals::Fight::default();
    fight.start(&v);
    let mut out = Intent::default();
    fight.tick(&v, &mut out);
    assert_eq!(out.goto, Some([-3.5, 0.0, 0.5]));
    assert_eq!(out.fight, None);
    assert!(!out.sprint);
}

#[test]
fn a_fighting_bot_away_from_an_edge_still_chases() {
    let enemies = [Seen { id: 1, pos: [8.0, 0.0, 0.0] }];
    let v = view([0.0; 3], &enemies);
    let mut fight = goals::Fight::default();
    fight.start(&v);
    let mut out = Intent::default();
    fight.tick(&v, &mut out);
    assert_eq!(out.goto, Some([8.0, 0.0, 0.0]));
    assert_eq!(out.fight, Some(1));
}

#[test]
fn a_bed_defender_at_an_edge_retreats_unless_the_bed_is_close() {
    let enemies = [Seen { id: 1, pos: [10.0, 64.0, 0.0] }];
    let mut v = view([0.0, 64.0, 0.0], &enemies);
    v.bed = Some([10, 64, 0]);
    v.edge = Some([-3.5, 64.0, 0.5]);
    let mut defend = goals::DefendBed;
    let mut out = Intent::default();
    defend.tick(&v, &mut out);
    assert_eq!(out.goto, Some([-3.5, 64.0, 0.5]));
    assert_eq!(out.fight, None);
    v.bed = Some([2, 64, 0]);
    let mut out = Intent::default();
    defend.tick(&v, &mut out);
    assert_eq!(out.goto, Some([2.5, 65.0, 0.5]));
}
