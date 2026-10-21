// SPDX-License-Identifier: MIT
use super::*;

fn look_at(host: &mut Rec, id: Id, [x, y, z]: Pos) {
    host.gazes.insert(
        id,
        (
            ARENA.to_owned(),
            [f64::from(x) + 0.5, f64::from(y) + 3.0, f64::from(z) + 0.5],
            [0.0, -1.0, 0.0],
        ),
    );
}

fn open(game: &mut Game, host: &mut Rec, id: Id, at: Pos) -> Option<String> {
    let before = host.menus.len();
    look_at(host, id, at);
    game.container_opened(host, id);
    host.menus
        .get(before)
        .filter(|(who, ..)| *who == id)
        .map(|(_, menu, _)| menu.clone())
}

const RED_CHEST: Pos = [2, 65, -8];
const RED_ENDER: Pos = [-2, 65, -8];
const BLUE_CHEST: Pos = [2, 65, 8];

fn three_teams(players: &[Id]) -> (Game, Rec) {
    let mut map = small(1);
    map.teams.push(Team {
        name: "Green".into(),
        color: "lime".into(),
        spawn: [12, 65, 0],
        yaw: 0.0,
        bed: [10, 65, 0],
        bed_facing: Facing::West,
        forge: [14, 65, 0],
    });
    map.validate().unwrap();
    let mut game = Game::new(Settings::default());
    let mut host = Rec::default();
    game.add_arena(1, ARENA, map, false).unwrap();
    ticks(&mut game, &mut host, 1);
    for &id in players {
        game.join(&mut host, who(id), None).unwrap();
    }
    game.start(&mut host, 1).unwrap();
    (game, host)
}

#[test]
fn every_island_gets_a_team_chest_and_an_ender_chest() {
    let (_game, host) = built(1);
    assert!(host.block(RED_CHEST).starts_with("minecraft:chest["));
    assert!(host.block(RED_ENDER).starts_with("minecraft:ender_chest["));
    assert!(host.block(BLUE_CHEST).starts_with("minecraft:chest["));
}

#[test]
fn a_team_chest_opens_for_its_team_only() {
    let (mut game, mut host) = running();
    assert_eq!(
        open(&mut game, &mut host, A, RED_CHEST).unwrap(),
        "chest:team:0"
    );
    assert_eq!(
        open(&mut game, &mut host, B, RED_CHEST).unwrap(),
        "chest:locked"
    );
    assert!(host.heard_any(B, "This chest belongs to another team."));
    assert_eq!(
        open(&mut game, &mut host, B, BLUE_CHEST).unwrap(),
        "chest:team:1"
    );
}

#[test]
fn an_island_that_never_had_a_team_stays_locked() {
    let (mut game, mut host) = three_teams(&[A, B]);
    let green_chest = [10, 65, 2];
    assert_eq!(
        open(&mut game, &mut host, A, green_chest).unwrap(),
        "chest:locked"
    );
    assert!(host.heard_any(A, "nobody lives on this island"));
}

#[test]
fn a_team_chest_opens_to_all_only_once_the_team_is_eliminated_for_good() {
    let (mut game, mut host) = running();
    game.died(&mut host, A);
    assert_eq!(
        open(&mut game, &mut host, B, RED_CHEST).unwrap(),
        "chest:locked"
    );

    let red_bed = "minecraft:red_bed[facing=south,part=foot,occupied=false]";
    game.broken(&mut host, Some(&who(B)), ARENA, [0, 65, -8], red_bed);
    assert_eq!(
        open(&mut game, &mut host, B, RED_CHEST).unwrap(),
        "chest:locked",
        "a respawning member keeps it locked"
    );
    ticks(&mut game, &mut host, 5 * SECOND);
    game.died(&mut host, A);
    assert!(game.arena(1).unwrap().game().unwrap().eliminated(0));
}

#[test]
fn eliminated_team_chest_is_open_to_a_third_team() {
    let (mut game, mut host) = three_teams(&[A, B, C]);
    let blue_bed = "minecraft:blue_bed[facing=north,part=foot,occupied=false]";
    game.broken(&mut host, Some(&who(A)), ARENA, [0, 65, 8], blue_bed);
    game.died(&mut host, B);
    assert!(game.arena(1).unwrap().game().unwrap().eliminated(1));
    assert_eq!(
        open(&mut game, &mut host, A, BLUE_CHEST).unwrap(),
        "chest:team:1"
    );
}

#[test]
fn ender_chests_are_per_player_and_apart_from_the_real_one() {
    let (mut game, mut host) = running();
    host.carry = vec![(0, stack("minecraft:iron_ingot", 12))];
    assert_eq!(
        open(&mut game, &mut host, A, RED_ENDER).unwrap(),
        "chest:ender"
    );
    let shown = host
        .menus
        .last()
        .unwrap()
        .2
        .iter()
        .map(|s| s.slot)
        .collect::<Vec<_>>();
    assert_eq!(shown, [31], "starts empty, whatever the real chest holds");

    game.menu_click(&mut host, A, "chest:ender", 31);
    game.menu_click(&mut host, A, "chest:ender", 31);
    assert_eq!(host.taken, [(A, "minecraft:iron_ingot".to_owned(), 12)]);
    let items = &host.menus.last().unwrap().2;
    assert_eq!(items[0].item, "minecraft:iron_ingot");
    assert_eq!(items[0].count, 12);

    assert_eq!(
        open(&mut game, &mut host, B, RED_ENDER).unwrap(),
        "chest:ender"
    );
    assert_eq!(
        host.menus.last().unwrap().2.len(),
        1,
        "Bo's ender chest is empty"
    );

    ticks(&mut game, &mut host, 1);
    game.menu_click(&mut host, A, "chest:ender", 0);
    assert_eq!(host.gave, [(A, "minecraft:iron_ingot".to_owned(), 12)]);
}

#[test]
fn team_chests_share_and_ender_contents_are_dropped_when_the_game_ends() {
    let (mut game, mut host) = running();
    host.carry = vec![(0, stack("minecraft:diamond", 3))];
    open(&mut game, &mut host, A, RED_CHEST).unwrap();
    game.menu_click(&mut host, A, "chest:team:0", 31);
    open(&mut game, &mut host, A, RED_ENDER).unwrap();
    ticks(&mut game, &mut host, 1);
    game.menu_click(&mut host, A, "chest:ender", 31);
    assert!(!game.arena(1).unwrap().game().unwrap().chests[0].is_empty());
    assert!(!game.arena(1).unwrap().game().unwrap().ender.is_empty());

    game.stop(&mut host, 1).unwrap();
    let ended = game.arena(1).unwrap().game().unwrap();
    assert!(ended.ender.is_empty());
    assert!(ended.chests.iter().all(Stash::is_empty));
}

#[test]
fn players_cannot_place_their_own_chests() {
    let (mut game, mut host) = running();
    game.placed(
        &mut host,
        Some(&who(A)),
        ARENA,
        [5, 65, -5],
        "minecraft:chest[facing=north]",
    );
    assert_eq!(host.block([5, 65, -5]), "minecraft:air");
}
