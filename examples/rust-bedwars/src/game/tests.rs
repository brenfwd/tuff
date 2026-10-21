// SPDX-License-Identifier: MIT
use std::collections::BTreeMap;

use tuff_plugin_sdk::text::plain;

use super::*;
use crate::economy::MINUTE;
use crate::map::{Facing, Fill, Team};

const ARENA: &str = "bedwars:arena_1";
const A: Id = 1;
const B: Id = 2;
const C: Id = 3;
const D: Id = 4;

#[derive(Default)]
struct Rec {
    told: Vec<(Id, String)>,
    blocks: BTreeMap<Pos, String>,
    writes: usize,
    teleports: Vec<(Id, [f64; 3])>,
    spawned: Vec<([f64; 3], String)>,
    swept: Vec<(Pos, String)>,
    given: Vec<String>,
    saved: Vec<Id>,
    restored: Vec<Id>,
    dirty: BTreeMap<usize, bool>,
    menus: Vec<(Id, String, Vec<MenuSlot>)>,
    carry: Vec<(u8, String)>,
    taken: Vec<(Id, String, u32)>,
    gave: Vec<(Id, String, u32)>,
    slots: Vec<(Id, u8, Option<String>)>,
    cleared: Vec<Id>,
    keeps: Vec<Id>,
    effects: Vec<(Id, String, u8)>,
    at: BTreeMap<Id, [f64; 3]>,
    titles: Vec<(Id, String, String)>,
    bars: Vec<(Id, String)>,
    side: Option<(String, Vec<String>)>,
    spectating: BTreeMap<Id, bool>,
    launched: Vec<(String, [f64; 3], String)>,
    shops: Vec<(String, [f64; 3], String)>,
    cleared_shops: usize,
    flying: BTreeMap<Id, [f64; 3]>,
    guarded: BTreeMap<usize, [Pos; 2]>,
    removed: Vec<Id>,
    gazes: BTreeMap<Id, (String, [f64; 3], [f64; 3])>,
    online: BTreeMap<String, Id>,
}

impl Host for Rec {
    fn tell(&mut self, to: Id, text: &str) {
        self.told.push((to, plain(text)));
    }

    fn set_block(&mut self, dimension: &str, at: Pos, state: &str) {
        assert_eq!(dimension, ARENA);
        self.blocks.insert(at, state.to_owned());
        self.writes += 1;
    }

    fn teleport(&mut self, who: Id, dimension: &str, at: [f64; 3], _yaw: f32) {
        assert_eq!(dimension, ARENA);
        self.teleports.push((who, at));
    }

    fn spawn_item(&mut self, _dimension: &str, at: [f64; 3], item: &str) {
        self.spawned.push((at, item.to_owned()));
    }

    fn items_near(&mut self, _dimension: &str, at: [f64; 3], item: &str) -> u32 {
        self.spawned
            .iter()
            .filter(|(p, i)| {
                (p[0] - at[0]).abs() <= 1.5 && (p[2] - at[2]).abs() <= 1.5 && i == item
            })
            .count() as u32
    }

    fn sweep(&mut self, _dimension: &str, at: Pos, item: &str) {
        self.swept.push((at, item.to_owned()));
        self.spawned.retain(|(p, i)| {
            i != item
                || (p[0] - f64::from(at[0]) - 0.5).abs() > 1.5
                || (p[2] - f64::from(at[2]) - 0.5).abs() > 1.5
        });
    }

    fn give_back(&mut self, item: &str) {
        self.given.push(item.to_owned());
    }

    fn save_state(&mut self, who: Id) {
        self.saved.push(who);
    }

    fn restore_state(&mut self, who: Id) {
        self.restored.push(who);
    }

    fn mark_dirty(&mut self, arena: usize, dirty: bool) {
        self.dirty.insert(arena, dirty);
    }

    fn open_menu(&mut self, who: Id, menu: &str, _rows: u8, _title: &str, items: &[MenuSlot]) {
        self.menus.push((who, menu.to_owned(), items.to_vec()));
    }

    fn carried(&mut self) -> Vec<(u8, String)> {
        self.carry.clone()
    }

    fn take(&mut self, who: Id, item: &str, count: u32) {
        self.taken.push((who, item.to_owned(), count));
    }

    fn give(&mut self, who: Id, item: &str, count: u32) {
        self.gave.push((who, item.to_owned(), count));
    }

    fn set_slot(&mut self, who: Id, slot: u8, stack: Option<&str>) {
        self.slots.push((who, slot, stack.map(str::to_owned)));
    }

    fn clear(&mut self, who: Id) {
        self.cleared.push(who);
    }

    fn keep_inventory(&mut self, who: Id, keep: bool) {
        assert!(keep);
        self.keeps.push(who);
    }

    fn effect(&mut self, who: Id, effect: &str, _ticks: i32, amplifier: u8) {
        self.effects.push((who, effect.to_owned(), amplifier));
    }

    fn position(&mut self, who: Id) -> Option<[f64; 3]> {
        self.at.get(&who).copied()
    }

    fn title(&mut self, who: Id, title: &str, subtitle: &str) {
        self.titles.push((who, plain(title), plain(subtitle)));
    }

    fn action_bar(&mut self, who: Id, text: &str) {
        self.bars.push((who, plain(text)));
    }

    fn sidebar(&mut self, lines: Option<(&str, &[String])>) {
        self.side = lines.map(|(t, l)| (plain(t), l.iter().map(|s| plain(s)).collect()));
    }

    fn spectate(&mut self, who: Id, on: bool) {
        self.spectating.insert(who, on);
    }

    fn clear_shopkeepers(&mut self, _dimension: &str, _bounds: [Pos; 2]) {
        self.cleared_shops += 1;
    }

    fn launch(&mut self, _dimension: &str, kind: &str, at: [f64; 3], snbt: &str) -> Option<Id> {
        let record = (kind.to_owned(), at, snbt.to_owned());
        if kind == "minecraft:villager" {
            self.shops.push(record);
            return Some(5000 + self.shops.len() as Id);
        }
        self.launched.push(record);
        let id = 1000 + self.launched.len() as Id;
        self.flying.insert(id, at);
        Some(id)
    }

    fn entity_at(&mut self, entity: Id) -> Option<[f64; 3]> {
        self.flying.get(&entity).copied()
    }

    fn block_at(&mut self, _dimension: &str, at: Pos) -> Option<String> {
        Some(
            self.blocks
                .get(&at)
                .cloned()
                .unwrap_or_else(|| "minecraft:air".into()),
        )
    }

    fn protect(&mut self, arena: usize, _dimension: &str, bounds: [Pos; 2], _beds: &[String]) {
        self.guarded.insert(arena, bounds);
    }

    fn unprotect(&mut self, arena: usize) {
        self.guarded.remove(&arena);
    }

    fn gaze(&mut self, who: Id) -> Option<(String, [f64; 3], [f64; 3])> {
        self.gazes.get(&who).cloned()
    }

    fn remove_entity(&mut self, entity: Id) {
        self.removed.push(entity);
    }

    fn player_named(&mut self, name: &str) -> Option<Id> {
        self.online.get(name).copied()
    }
}

impl Rec {
    fn heard(&self, id: Id) -> Vec<&str> {
        self.told
            .iter()
            .filter(|(to, _)| *to == id)
            .map(|(_, t)| t.as_str())
            .collect()
    }

    fn heard_any(&self, id: Id, part: &str) -> bool {
        self.heard(id).iter().any(|t| t.contains(part))
    }

    fn block(&self, at: Pos) -> &str {
        self.blocks.get(&at).map_or("", String::as_str)
    }
}

fn team(name: &str, color: &str, z: i32, facing: Facing) -> Team {
    let s = z.signum();
    Team {
        name: name.into(),
        color: color.into(),
        spawn: [0, 65, z],
        yaw: 0.0,
        bed: [0, 65, z - 2 * s],
        bed_facing: facing,
        forge: [0, 65, z + 2 * s],
    }
}

fn small(team_size: usize) -> Map {
    let map = Map {
        name: "small".into(),
        team_size,
        teams: vec![
            team("Red", "red", -10, Facing::South),
            team("Blue", "blue", 10, Facing::North),
        ],
        diamonds: vec![[8, 65, 0]],
        emeralds: vec![[0, 65, 0]],
        waiting: [0, 90, 0],
        bounds: [[-16, 60, -16], [16, 100, 16]],
        build_limit: 80,
        protect: 1,
        fills: vec![Fill {
            from: [-12, 64, -12],
            to: [12, 64, 12],
            block: "minecraft:stone".into(),
        }],
    };
    map.validate().unwrap();
    map
}

fn who(id: Id) -> Who {
    Who::new(id, ["", "Alex", "Bo", "Cy", "Di"][id as usize])
}

fn ticks(game: &mut Game, host: &mut Rec, n: u64) {
    for _ in 0..n {
        game.tick(host);
    }
}

fn built(team_size: usize) -> (Game, Rec) {
    let mut game = Game::new(Settings::default());
    let mut host = Rec::default();
    game.add_arena(1, ARENA, small(team_size), false).unwrap();
    ticks(&mut game, &mut host, 1);
    assert!(matches!(
        game.arena(1).unwrap().phase,
        Phase::Waiting { start_at: None }
    ));
    (game, host)
}

/// A started 1v1: Alex is red, Bo is blue.
fn running() -> (Game, Rec) {
    let (mut game, mut host) = built(1);
    game.join(&mut host, who(A), None).unwrap();
    game.join(&mut host, who(B), None).unwrap();
    game.start(&mut host, 1).unwrap();
    (game, host)
}

fn running_game(game: &Game) -> &Match {
    match &game.arena(1).unwrap().phase {
        Phase::Running(g) => g,
        other => panic!("not running: {other:?}"),
    }
}

#[test]
fn an_arena_builds_its_map_then_waits() {
    let mut game = Game::new(Settings {
        writes_per_tick: 300,
        ..Settings::default()
    });
    let mut host = Rec::default();
    game.add_arena(1, ARENA, small(1), false).unwrap();
    let total = small(1).blocks().len();
    ticks(&mut game, &mut host, 1);
    assert_eq!(host.writes, 300);
    assert_eq!(game.arena(1).unwrap().phase, Phase::Building);
    ticks(&mut game, &mut host, (total / 300) as u64 + 1);
    assert_eq!(host.writes, total);
    assert!(matches!(
        game.arena(1).unwrap().phase,
        Phase::Waiting { .. }
    ));
    assert_eq!(host.dirty.get(&1), Some(&false));
    assert_eq!(
        host.block([0, 65, -8]),
        "minecraft:red_bed[facing=south,part=foot,occupied=false]"
    );
    assert_eq!(
        host.block([0, 65, -7]),
        "minecraft:red_bed[facing=south,part=head,occupied=false]"
    );
    assert_eq!(host.block([0, 89, 0]), "minecraft:glass");
}

#[test]
fn a_waiting_arena_wiped_by_regeneration_rebuilds_itself() {
    let (mut game, mut host) = built(1);
    game.join(&mut host, who(A), None).unwrap();
    ticks(&mut game, &mut host, SECOND);
    assert!(matches!(
        game.arena(1).unwrap().phase,
        Phase::Waiting { .. }
    ));
    host.blocks.clear();
    host.teleports.clear();
    ticks(&mut game, &mut host, 2 * SECOND);
    assert!(matches!(
        game.arena(1).unwrap().phase,
        Phase::Waiting { .. }
    ));
    assert_eq!(host.block([0, 89, 0]), "minecraft:glass");
    assert_eq!(
        host.block([0, 65, -8]),
        "minecraft:red_bed[facing=south,part=foot,occupied=false]"
    );
    assert!(host.teleports.iter().any(|(id, _)| *id == A));
}

#[test]
fn a_dirty_arena_is_cleared_box_and_all_first() {
    let mut game = Game::new(Settings {
        writes_per_tick: 1 << 20,
        ..Settings::default()
    });
    let mut host = Rec::default();
    game.add_arena(1, ARENA, small(1), true).unwrap();
    ticks(&mut game, &mut host, 1);
    let volume = 33 * 41 * 33;
    assert_eq!(host.writes, volume + small(1).blocks().len());
    assert_eq!(host.block([16, 100, 16]), "minecraft:air");
    assert_eq!(host.block([-16, 60, -16]), "minecraft:air");
    assert_eq!(host.block([12, 64, 12]), "minecraft:stone");
}

#[test]
fn joining_saves_state_moves_to_the_platform_and_counts_down() {
    let (mut game, mut host) = built(2);
    let reply = game.join(&mut host, who(A), None).unwrap();
    assert_eq!(reply, "You joined arena 1 (small)");
    assert_eq!(host.saved, [A]);
    assert_eq!(host.teleports, [(A, [0.5, 90.0, 0.5])]);
    assert!(host.heard_any(A, "Alex has joined (1/4)!"));
    ticks(&mut game, &mut host, 5);
    assert!(!host.heard_any(A, "starts in"), "one player is not enough");

    game.join(&mut host, who(B), Some(1)).unwrap();
    ticks(&mut game, &mut host, 1);
    assert!(host.heard_any(B, "The game starts in 20 seconds!"));
    ticks(&mut game, &mut host, 10 * SECOND - 1);
    assert!(!host.heard_any(A, "The game starts in 10 seconds!"));
    ticks(&mut game, &mut host, 1);
    assert!(host.heard_any(A, "The game starts in 10 seconds!"));

    game.leave(&mut host, B, true).unwrap();
    assert_eq!(host.restored, [B]);
    ticks(&mut game, &mut host, 1);
    assert!(host.heard_any(A, "the countdown stopped"));
    assert!(host.heard_any(A, "Bo has quit!"));

    for id in [B, C, D] {
        game.join(&mut host, who(id), None).unwrap();
    }
    assert_eq!(
        game.join(&mut host, Who::new(9, "Eve"), None).unwrap_err(),
        "No arena has room; try again after the next game"
    );
    ticks(&mut game, &mut host, 1);
    assert!(
        host.heard_any(D, "The game starts in 10 seconds!"),
        "a full arena waits 10 s"
    );
    ticks(&mut game, &mut host, 10 * SECOND);
    let g = running_game(&game);
    let teams: Vec<usize> = [A, B, C, D].iter().map(|id| g.members[id].team).collect();
    assert_eq!(teams, [0, 1, 0, 1], "teams fill evenly in join order");
    assert!(host.heard_any(C, "You are on the Red team."));
    assert_eq!(host.dirty.get(&1), Some(&true));
}

#[test]
fn starting_assigns_teams_clears_the_platform_and_empty_beds() {
    let mut game = Game::new(Settings::default());
    let mut host = Rec::default();
    game.add_arena(1, ARENA, crate::map::classic("classic", 1), false)
        .unwrap();
    ticks(&mut game, &mut host, 1);
    game.join(&mut host, who(A), None).unwrap();
    assert_eq!(
        game.start(&mut host, 1).unwrap_err(),
        "A game needs at least 2 players"
    );
    game.join(&mut host, who(B), None).unwrap();
    assert_eq!(
        game.start(&mut host, 1).unwrap(),
        "Started arena 1 (classic)"
    );
    assert_eq!(host.block([0, 109, 0]), "minecraft:air", "platform gone");
    assert_eq!(host.block([0, 65, 37]), "minecraft:air", "green has nobody");
    assert!(host.block([0, 65, -37]).starts_with("minecraft:red_bed"));
    let g = running_game(&game);
    assert_eq!(
        g.teams.iter().map(|t| t.bed).collect::<Vec<_>>(),
        [true, true, false, false]
    );
    assert!(host.teleports.contains(&(A, [0.5, 65.0, -41.5])));
    assert!(host.teleports.contains(&(B, [42.5, 65.0, 0.5])));
    assert_eq!(
        game.join(&mut host, who(C), Some(1)).unwrap_err(),
        "A game is running in arena 1 (classic)"
    );
    assert_eq!(
        game.join(&mut host, who(A), None).unwrap_err(),
        "You are already in arena 1 (classic); leave it with /bedwars leave"
    );
}

#[test]
fn forges_and_generators_drop_on_their_clocks_up_to_their_caps() {
    let (mut game, mut host) = running();
    let red_forge = [0.5, 65.0, -11.5];
    let count = |host: &Rec, at: [f64; 3], item: &str| {
        host.spawned
            .iter()
            .filter(|(p, i)| {
                (p[0] - at[0]).abs() <= 1.5 && (p[2] - at[2]).abs() <= 1.5 && i == item
            })
            .count()
    };
    ticks(&mut game, &mut host, 1);
    assert_eq!(count(&host, red_forge, "minecraft:iron_ingot"), 1);
    assert_eq!(count(&host, red_forge, "minecraft:gold_ingot"), 1);
    ticks(&mut game, &mut host, 25 * 10);
    assert_eq!(count(&host, red_forge, "minecraft:iron_ingot"), 11);
    assert_eq!(count(&host, red_forge, "minecraft:gold_ingot"), 2);
    assert_eq!(count(&host, [8.5, 65.0, 0.5], "minecraft:diamond"), 0);
    ticks(&mut game, &mut host, 30 * SECOND);
    assert_eq!(count(&host, [8.5, 65.0, 0.5], "minecraft:diamond"), 1);
    assert_eq!(
        count(&host, red_forge, "minecraft:emerald"),
        0,
        "no emerald forge yet"
    );

    ticks(&mut game, &mut host, 5 * MINUTE);
    assert_eq!(
        count(&host, red_forge, "minecraft:iron_ingot"),
        48,
        "iron caps at 48"
    );
    assert_eq!(count(&host, [8.5, 65.0, 0.5], "minecraft:diamond"), 4);
    assert_eq!(count(&host, [0.5, 65.0, 0.5], "minecraft:emerald"), 2);

    if let Phase::Running(g) = &mut game.arenas[0].phase {
        g.teams[0].forge = 3;
    }
    ticks(&mut game, &mut host, SECOND);
    assert_eq!(
        count(&host, red_forge, "minecraft:emerald"),
        1,
        "Emerald Forge"
    );
    ticks(&mut game, &mut host, 74 * SECOND);
    assert_eq!(count(&host, red_forge, "minecraft:emerald"), 1);
    ticks(&mut game, &mut host, 2 * SECOND);
    assert_eq!(count(&host, red_forge, "minecraft:emerald"), 2);
}

#[test]
fn the_timeline_upgrades_generators_breaks_beds_and_ends_in_a_draw() {
    let (mut game, mut host) = running();
    ticks(&mut game, &mut host, 6 * MINUTE);
    assert!(host.heard_any(A, "Diamond Generators have been upgraded to Tier II"));
    assert_eq!(running_game(&game).passed, 1);
    let status = game.status(1).unwrap().join("\n");
    assert!(
        plain(&status).contains("Next: Emerald II in 6:00"),
        "{status}"
    );
    ticks(&mut game, &mut host, 24 * MINUTE);
    assert!(host.heard_any(B, "Emerald Generators have been upgraded to Tier III"));
    assert!(host.heard_any(B, "All beds have been destroyed!"));
    assert!(running_game(&game).teams.iter().all(|t| !t.bed));
    assert_eq!(host.block([0, 65, -8]), "minecraft:air");
    ticks(&mut game, &mut host, 20 * MINUTE);
    assert!(host.heard_any(A, "Time is up: the game ends in a draw."));
    assert!(host.heard_any(A, "GAME OVER: nobody wins."));
    assert!(matches!(game.arena(1).unwrap().phase, Phase::Ending { .. }));
}

#[test]
fn only_enemies_break_beds_and_only_placed_blocks_break() {
    let (mut game, mut host) = running();
    let red_bed = "minecraft:red_bed[facing=south,part=head,occupied=false]";
    game.broken(&mut host, Some(&who(A)), ARENA, [0, 65, -7], red_bed);
    assert!(host.heard_any(A, "You can't break your own bed!"));
    assert_eq!(host.block([0, 65, -7]), red_bed);
    assert_eq!(host.swept.last().unwrap().1, "minecraft:red_bed");
    assert!(running_game(&game).teams[0].bed);

    game.broken(&mut host, Some(&who(B)), ARENA, [0, 65, -8], red_bed);
    assert!(!running_game(&game).teams[0].bed);
    assert_eq!(host.block([0, 65, -7]), "minecraft:air");
    assert!(host.heard_any(A, "BED DESTRUCTION > Red Bed was destroyed by Bo"));
    assert!(host.heard_any(A, "Your bed was destroyed! You will no longer respawn!"));
    assert!(!host.heard_any(B, "Your bed was destroyed"));

    game.broken(
        &mut host,
        Some(&who(A)),
        ARENA,
        [3, 64, 3],
        "minecraft:stone",
    );
    assert_eq!(host.block([3, 64, 3]), "minecraft:stone");
    assert_eq!(
        host.swept.last().unwrap(),
        &([3, 64, 3], "minecraft:stone".to_owned())
    );
    assert!(host.heard_any(A, "You can only break blocks placed by a player!"));

    game.placed(
        &mut host,
        Some(&who(A)),
        ARENA,
        [3, 65, 3],
        "minecraft:red_wool",
    );
    assert!(running_game(&game).placed.contains(&[3, 65, 3]));
    let writes = host.writes;
    game.broken(
        &mut host,
        Some(&who(B)),
        ARENA,
        [3, 65, 3],
        "minecraft:red_wool",
    );
    assert_eq!(host.writes, writes, "a placed block just breaks");
    assert!(running_game(&game).placed.is_empty());

    game.broken(
        &mut host,
        None,
        "minecraft:overworld",
        [3, 64, 3],
        "minecraft:stone",
    );
    assert_eq!(host.writes, writes, "other dimensions are not ours");
}

#[test]
fn placing_is_refused_outside_the_box_near_spawns_and_for_onlookers() {
    let (mut game, mut host) = running();
    for (at, reason) in [
        ([0, 81, 0], "You can't place blocks here!"),
        ([0, 65, -11], "You can't place blocks here!"),
        ([17, 65, 0], "You can't place blocks here!"),
    ] {
        game.placed(
            &mut host,
            Some(&who(A)),
            ARENA,
            at,
            "minecraft:red_wool[x=1]",
        );
        assert_eq!(host.block(at), "minecraft:air");
        assert_eq!(host.given.last().unwrap(), "minecraft:red_wool");
        assert_eq!(host.heard(A).last().unwrap(), &reason);
    }
    game.placed(
        &mut host,
        Some(&who(C)),
        ARENA,
        [2, 65, 2],
        "minecraft:dirt",
    );
    assert_eq!(host.heard(C).last().unwrap(), &"You can't build here!");
    assert!(running_game(&game).placed.is_empty());
}

#[test]
fn breaking_and_placing_before_a_game_is_undone() {
    let (mut game, mut host) = built(1);
    game.broken(
        &mut host,
        Some(&who(A)),
        ARENA,
        [0, 64, 0],
        "minecraft:stone",
    );
    assert_eq!(host.block([0, 64, 0]), "minecraft:stone");
    game.placed(
        &mut host,
        Some(&who(A)),
        ARENA,
        [0, 65, 0],
        "minecraft:dirt",
    );
    assert_eq!(host.block([0, 65, 0]), "minecraft:air");
    assert_eq!(host.given, ["minecraft:dirt"]);
}

#[test]
fn deaths_respawn_until_the_bed_is_gone_then_eliminate_and_decide() {
    let (mut game, mut host) = running();
    game.died(&mut host, A);
    assert!(host.heard_any(A, "You will respawn in 5 seconds!"));
    assert!(host.heard_any(B, "Alex died."));
    game.died(&mut host, A);
    assert_eq!(
        host.heard(B)
            .iter()
            .filter(|t| t.contains("Alex died"))
            .count(),
        1,
        "a respawning player cannot die again"
    );
    ticks(&mut game, &mut host, 5 * SECOND);
    assert_eq!(host.teleports.last().unwrap(), &(A, [0.5, 65.0, -9.5]));
    assert!(host.heard_any(A, "You have respawned!"));

    let red_bed = "minecraft:red_bed[facing=south,part=foot,occupied=false]";
    game.broken(&mut host, Some(&who(B)), ARENA, [0, 65, -8], red_bed);
    game.placed(
        &mut host,
        Some(&who(B)),
        ARENA,
        [5, 65, 5],
        "minecraft:blue_wool",
    );
    game.died(&mut host, A);
    assert!(host.heard_any(B, "Alex died. FINAL KILL!"));
    assert!(host.heard_any(A, "You have been eliminated!"));
    assert!(host.heard_any(B, "TEAM ELIMINATED > Red has been eliminated!"));
    assert!(host.heard_any(B, "VICTORY! Blue wins: Bo"));
    assert!(matches!(game.arena(1).unwrap().phase, Phase::Ending { .. }));
    assert!(game.arena_of(A).is_none());
    assert_eq!(
        game.join(&mut host, who(A), None).unwrap_err(),
        "You can join again in a moment"
    );

    ticks(&mut game, &mut host, 5 * SECOND);
    assert_eq!(host.restored, [A], "the eliminated player goes back");
    ticks(&mut game, &mut host, 5 * SECOND);
    assert_eq!(host.restored, [A, A, B], "everyone goes back at the end");
    assert_eq!(game.arena(1).unwrap().phase, Phase::Building);
    ticks(&mut game, &mut host, 1);
    assert_eq!(
        host.block([5, 65, 5]),
        "minecraft:air",
        "placed blocks are cleared"
    );
    assert!(
        host.block([0, 65, -8]).starts_with("minecraft:red_bed"),
        "beds return"
    );
    assert_eq!(host.block([0, 89, 0]), "minecraft:glass");
    assert!(matches!(
        game.arena(1).unwrap().phase,
        Phase::Waiting { .. }
    ));
    assert_eq!(host.dirty.get(&1), Some(&false));
    game.join(&mut host, who(A), None).unwrap();
}

#[test]
fn leaving_or_disconnecting_mid_game_forfeits() {
    let (mut game, mut host) = running();
    assert_eq!(
        game.leave(&mut host, C, true).unwrap_err(),
        "You are not in a game"
    );
    assert_eq!(
        game.leave(&mut host, B, false).unwrap(),
        "You left arena 1 (small)"
    );
    assert!(
        host.restored.is_empty(),
        "a disconnected player is put back on return"
    );
    assert!(host.heard_any(A, "Bo left the game."));
    assert!(host.heard_any(A, "VICTORY! Red wins: Alex"));

    let (mut game, mut host) = running();
    game.stop(&mut host, 1).unwrap();
    assert!(host.heard_any(A, "The game was stopped."));
    assert_eq!(
        game.stop(&mut host, 1).unwrap_err(),
        "No game runs in arena 1 (small)"
    );
    game.shutdown(&mut host);
    assert_eq!(host.restored, [A, B]);
}

#[test]
fn arenas_can_be_replaced_only_when_empty_and_listed() {
    let (mut game, mut host) = built(1);
    game.join(&mut host, who(A), None).unwrap();
    assert_eq!(
        game.add_arena(1, ARENA, small(2), false).unwrap_err(),
        "Players are in arena 1"
    );
    let list = plain(&game.list().join("\n"));
    assert_eq!(list, "Arena 1 small: waiting 1/2");
    game.remove_arena(&mut host, 1).unwrap();
    assert_eq!(host.restored, [A]);
    assert!(game.arenas.is_empty());
    assert_eq!(game.status(1).unwrap_err(), "There is no arena 1");
}

fn stack(id: &str, count: u32) -> String {
    crate::shop::stack(id, count, &[])
}

fn click_offer(game: &mut Game, host: &mut Rec, id: Id, category: &str, key: &str) {
    let offer = crate::shop::find(key).unwrap();
    let slot = crate::shop::offer_slots(crate::shop::Category::of(category).unwrap())
        .into_iter()
        .find(|(_, o)| o.key == offer.key)
        .unwrap()
        .0;
    game.menu_click(host, id, &format!("shop:{category}"), slot);
}

#[test]
fn the_shop_lists_a_category_and_switches_tabs() {
    let (mut game, mut host) = running();
    assert_eq!(host.keeps, [A, B]);
    game.open_shop(&mut host, A, crate::shop::Category::Melee);
    let (who, menu, items) = host.menus.last().unwrap().clone();
    assert_eq!((who, menu.as_str()), (A, "shop:melee"));
    assert!(
        items
            .iter()
            .any(|s| s.name == "Stone Sword" && s.lore == ["Cost: 10 Iron"])
    );
    assert!(items.iter().any(|s| s.name == "melee" && s.glint));
    game.menu_click(
        &mut host,
        A,
        "shop:melee",
        crate::shop::tab_slot(crate::shop::Category::Ranged),
    );
    assert_eq!(host.menus.last().unwrap().1, "shop:ranged");
    game.menu_click(&mut host, A, "shop:ranged", crate::shop::UPGRADES_SLOT);
    assert_eq!(host.menus.last().unwrap().1, "upgrades");
}

#[test]
fn buying_takes_the_price_gives_the_goods_and_cannot_spend_twice() {
    let (mut game, mut host) = running();
    host.carry = vec![(36, stack("minecraft:iron_ingot", 6))];
    click_offer(&mut game, &mut host, A, "blocks", "wool");
    assert_eq!(host.taken, [(A, "minecraft:iron_ingot".to_owned(), 4)]);
    assert_eq!(host.gave, [(A, "minecraft:red_wool".to_owned(), 16)]);
    click_offer(&mut game, &mut host, A, "blocks", "wool");
    assert_eq!(host.taken.len(), 1, "the first payment is still pending");
    assert!(host.heard_any(A, "You need 2 more Iron"));
    game.tick(&mut host);
    click_offer(&mut game, &mut host, A, "blocks", "wool");
    assert_eq!(host.taken.len(), 2, "a new tick sees the inventory again");
}

#[test]
fn gear_is_placed_enchanted_and_armor_only_upgrades() {
    let (mut game, mut host) = running();
    host.carry = vec![(36, stack("minecraft:gold_ingot", 40))];
    click_offer(&mut game, &mut host, A, "ranged", "power_bow");
    let (_, slot, bow) = host.slots.last().unwrap().clone();
    assert_eq!(slot, 37);
    assert!(bow.unwrap().contains("\"minecraft:power\":1"));
    host.carry.push((37, stack("minecraft:iron_ingot", 100)));
    host.carry.push((8, stack("minecraft:iron_boots", 1)));
    host.slots.clear();
    click_offer(&mut game, &mut host, A, "armor", "chainmail");
    assert!(host.slots.is_empty());
    assert!(host.heard_any(A, "already wear"));
    click_offer(&mut game, &mut host, A, "armor", "iron_armor");
    assert!(host.slots.is_empty(), "same tier");
}

#[test]
fn team_upgrades_cost_diamonds_and_enchant_gear() {
    let (mut game, mut host) = running();
    host.carry = vec![
        (36, stack("minecraft:diamond", 5)),
        (37, stack("minecraft:wooden_sword", 1)),
        (6, stack("minecraft:leather_chestplate", 1)),
    ];
    let slot = crate::shop::upgrade_slot(0);
    game.menu_click(&mut host, A, "upgrades", slot);
    assert_eq!(host.taken, [(A, "minecraft:diamond".to_owned(), 4)]);
    assert!(
        host.slots.iter().any(
            |(_, s, st)| *s == 37 && st.as_ref().unwrap().contains("\"minecraft:sharpness\":1")
        )
    );
    assert!(host.heard_any(A, "Alex purchased Sharpened Swords"));
    assert_eq!(running_game(&game).teams[0].tier("sharpness"), 1);
    game.tick(&mut host);
    game.menu_click(&mut host, A, "upgrades", slot);
    assert!(host.heard_any(A, "maxed"), "{:?}", host.heard(A));
    let protection = crate::shop::upgrade_slot(1);
    host.carry = vec![(36, stack("minecraft:diamond", 1))];
    game.tick(&mut host);
    game.menu_click(&mut host, A, "upgrades", protection);
    assert!(host.heard_any(A, "You need 1 more Diamond"));
    host.carry = vec![(36, stack("minecraft:diamond", 8))];
    for _ in 0..3 {
        game.tick(&mut host);
        game.menu_click(&mut host, A, "upgrades", crate::shop::upgrade_slot(3));
    }
    assert_eq!(running_game(&game).teams[0].forge, 3);
}

#[test]
fn a_death_moves_resources_to_the_killer_and_keeps_gear() {
    let (mut game, mut host) = running();
    host.carry = vec![
        (36, stack("minecraft:iron_ingot", 30)),
        (37, stack("minecraft:iron_ingot", 5)),
        (38, stack("minecraft:diamond", 2)),
        (39, stack("minecraft:stone", 64)),
        (40, stack("minecraft:wooden_pickaxe", 1)),
        (7, stack("minecraft:chainmail_leggings", 1)),
    ];
    game.died_by(&mut host, A, Some(B));
    assert_eq!(host.cleared, [A]);
    assert_eq!(
        host.gave,
        [
            (B, "minecraft:iron_ingot".to_owned(), 35),
            (B, "minecraft:diamond".to_owned(), 2)
        ]
    );
    let kept: Vec<(u8, String)> = host
        .slots
        .iter()
        .filter(|(who, slot, _)| *who == A && *slot != 5 && *slot != 6 && *slot != 8)
        .map(|(_, s, st)| (*s, shop_id(st.as_ref().unwrap())))
        .filter(|(s, _)| *s >= 36 || *s == 7)
        .collect();
    assert!(kept.contains(&(40, "minecraft:wooden_pickaxe".into())));
    assert!(kept.contains(&(7, "minecraft:chainmail_leggings".into())));
    assert!(kept.iter().any(|(_, id)| id == "minecraft:wooden_sword"));
    assert!(kept.iter().all(|(_, id)| id != "minecraft:stone"));
    assert!(host.heard_any(B, "+35 Iron"));
}

fn shop_id(stack: &str) -> String {
    crate::shop::snbt_id(stack).unwrap().to_owned()
}

#[test]
fn only_an_enemy_collects_and_a_void_death_loses_the_loot() {
    let (mut game, mut host) = running();
    host.carry = vec![(36, stack("minecraft:gold_ingot", 9))];
    game.died_by(&mut host, A, None);
    assert!(host.gave.is_empty());
    game.died_by(&mut host, A, Some(A));
    assert!(host.gave.is_empty());
}

#[test]
fn upgrade_effects_haste_heal_pool_and_traps() {
    let (mut game, mut host) = running();
    {
        let Phase::Running(g) = &mut game.arenas[0].phase else {
            panic!()
        };
        for key in ["haste", "heal_pool", "trap"] {
            g.teams[0].upgrades.insert(key.into(), 1);
        }
    }
    let home = center(game.arena(1).unwrap().map.teams[0].spawn);
    host.at.insert(A, home);
    host.at.insert(B, [home[0] + 2.0, home[1], home[2]]);
    while game.now % 40 != 39 {
        game.tick(&mut host);
    }
    host.effects.clear();
    game.tick(&mut host);
    let kinds: Vec<(Id, &str)> = host
        .effects
        .iter()
        .map(|(i, e, _)| (*i, e.as_str()))
        .collect();
    assert!(kinds.contains(&(A, "minecraft:haste")));
    assert!(kinds.contains(&(A, "minecraft:regeneration")));
    assert!(kinds.contains(&(B, "minecraft:blindness")));
    assert!(host.heard_any(A, "walked into your trap"));
    assert_eq!(
        running_game(&game).teams[0].tier("trap"),
        0,
        "the trap is spent"
    );
}

#[test]
fn deaths_spectate_with_a_countdown_and_the_sidebar_shows_the_game() {
    let (mut game, mut host) = running();
    let to_second = SECOND - game.now % SECOND;
    ticks(&mut game, &mut host, to_second);
    let (title, lines) = host.side.clone().expect("a sidebar");
    assert_eq!(title, "BED WARS");
    assert!(lines[0].starts_with("Diamond II in 5:5"), "{lines:?}");
    assert!(lines.contains(&"Red: Y".to_owned()), "{lines:?}");

    game.died(&mut host, A);
    assert_eq!(host.spectating.get(&A), Some(&true));
    assert_eq!(host.titles.last().unwrap().1, "YOU DIED!");
    host.bars.clear();
    ticks(&mut game, &mut host, 2 * SECOND);
    assert!(
        host.bars
            .iter()
            .any(|(id, t)| *id == A && t == "Respawning in 4...")
    );
    ticks(&mut game, &mut host, 3 * SECOND);
    assert_eq!(host.spectating.get(&A), Some(&false));
    assert_eq!(host.titles.last().unwrap().1, "RESPAWNED!");

    let red_bed = "minecraft:red_bed[facing=south,part=foot,occupied=false]";
    game.broken(&mut host, Some(&who(B)), ARENA, [0, 65, -8], red_bed);
    assert!(
        host.titles
            .iter()
            .any(|(id, t, _)| *id == A && t == "BED DESTROYED!")
    );
    game.died(&mut host, A);
    ticks(&mut game, &mut host, 1);
    assert!(
        host.titles
            .iter()
            .any(|(id, t, _)| *id == B && t == "VICTORY!")
    );
    ticks(&mut game, &mut host, SECOND);
    assert!(
        host.side
            .as_ref()
            .is_some_and(|(_, l)| l.contains(&"Red: X".to_owned()))
    );
}

#[test]
fn a_fire_charge_launches_a_fireball_along_the_look_and_costs_one() {
    let (mut game, mut host) = running();
    host.at.insert(A, [0.0, 65.0, -10.0]);
    game.used(&mut host, &who(A), ARENA, FIRE_CHARGE, [0.0, 0.0]);
    assert_eq!(host.taken, vec![(A, FIRE_CHARGE.to_owned(), 1)]);
    let (kind, at, snbt) = &host.launched[0];
    assert_eq!(kind, "minecraft:fireball");
    assert!((at[2] - (-10.0 + 1.0)).abs() < 1e-9 && (at[1] - 66.62).abs() < 1e-9);
    assert!(
        snbt.contains("0.1d]") && snbt.contains("acceleration_power:0.1d"),
        "{snbt}"
    );
    assert!(snbt.contains("Owner:[I;0,0,0,1]"), "{snbt}");

    game.used(&mut host, &who(C), ARENA, FIRE_CHARGE, [0.0, 0.0]);
    assert_eq!(host.launched.len(), 1, "onlookers cannot throw");
}

#[test]
fn a_bridge_egg_lays_wool_under_itself_in_the_team_color_for_a_while() {
    let (mut game, mut host) = running();
    host.at.insert(A, [0.0, 65.0, -10.0]);
    game.used(&mut host, &who(A), ARENA, EGG, [0.0, 0.0]);
    assert_eq!(host.launched[0].0, "minecraft:egg");
    let egg = 1001;
    host.flying.insert(egg, [3.5, 66.2, 0.5]);
    ticks(&mut game, &mut host, 1);
    assert_eq!(host.block([3, 65, 0]), "minecraft:red_wool");
    assert!(running_game(&game).placed.contains(&[3, 65, 0]));
    host.flying.insert(egg, [3.5, 66.2, 1.5]);
    host.blocks.insert([3, 65, 1], "minecraft:stone".into());
    ticks(&mut game, &mut host, 1);
    assert_eq!(
        host.block([3, 65, 1]),
        "minecraft:stone",
        "solid blocks stay"
    );
    host.flying.insert(egg, [40.5, 66.2, 0.5]);
    ticks(&mut game, &mut host, 1);
    assert_eq!(host.block([40, 65, 0]), "", "outside the box");
    host.flying.remove(&egg);
    ticks(&mut game, &mut host, 1);
    assert!(game.bridges.is_empty(), "a landed egg stops bridging");
}

#[test]
fn tnt_primes_itself_when_placed_and_the_box_is_guarded_while_running() {
    let (mut game, mut host) = running();
    assert_eq!(host.guarded[&1], [[-16, 60, -16], [16, 100, 16]]);
    game.placed(&mut host, Some(&who(A)), ARENA, [2, 65, 2], "minecraft:tnt");
    assert_eq!(host.block([2, 65, 2]), "minecraft:air");
    let (kind, at, snbt) = &host.launched[0];
    assert_eq!(kind, "minecraft:tnt");
    assert_eq!(*at, [2.5, 65.0, 2.5]);
    assert!(
        snbt.contains("fuse:40s") && snbt.contains("owner:[I;0,0,0,1]"),
        "{snbt}"
    );
    assert!(running_game(&game).placed.is_empty());
    game.stop(&mut host, 1).unwrap();
    assert!(host.guarded.is_empty());
}

mod bots;
mod chests;

#[test]
fn exactly_one_shopkeeper_per_team_island() {
    let (game, host) = built(1);
    let map = &game.arena(1).unwrap().map;
    let spots = map.shop_spots();
    assert_eq!(spots.len(), map.teams.len());
    let shops: Vec<_> = host
        .shops
        .iter()
        .filter(|(kind, _, snbt)| kind == "minecraft:villager" && snbt.contains("bedwars_shop"))
        .collect();
    assert_eq!(shops.len(), map.teams.len());
    assert!(
        shops
            .iter()
            .all(|(_, _, snbt)| !snbt.contains("PersistenceRequired"))
    );
    for spot in spots {
        assert_eq!(shops.iter().filter(|(_, at, _)| *at == spot).count(), 1);
    }
}

#[test]
fn building_clears_saved_shopkeepers_first() {
    let (_game, host) = built(1);
    assert_eq!(host.cleared_shops, 1);
}
