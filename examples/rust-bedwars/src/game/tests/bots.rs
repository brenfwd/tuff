// SPDX-License-Identifier: MIT
use super::*;
use crate::bot::{self, Difficulty};
use crate::game::bots::next_buy;
use tuff_bots_api::control;

/// The uuid the fake player of the first bot gets.
const BOT: Id = 50;
/// The bots plugin's id for the first bot.
const HANDLE: control::BotId = 7;

fn spawns(game: &Game) -> Vec<(u64, control::SpawnSpec)> {
    game.bot_calls
        .iter()
        .filter_map(|c| match c {
            BotCall::Spawn(n, spec) => Some((*n, spec.clone())),
            _ => None,
        })
        .collect()
}

fn walks(game: &Game) -> Vec<control::Process> {
    game.bot_calls
        .iter()
        .filter_map(|c| match c {
            BotCall::Process(HANDLE, p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

fn near(process: &control::Process) -> Option<[i32; 3]> {
    match process {
        control::Process::Goto(control::Goal::Leaf(control::LeafGoal::Near(goal))) => {
            Some([goal.pos.x, goal.pos.y, goal.pos.z])
        }
        _ => None,
    }
}

/// Bot 0 was spawned by the bots plugin and is online as [`BOT`].
fn online(game: &mut Game, host: &mut Rec) {
    game.bot_spawned(host, 0, Ok(HANDLE));
    host.online.insert(bot::name(0), BOT);
    ticks(game, host, 1);
}

/// A started 1v1 of Alex (red) against a bot (blue), one tick in.
fn against(difficulty: Difficulty) -> (Game, Rec) {
    let (mut game, mut host) = built(1);
    game.bots_present = true;
    game.join(&mut host, who(A), None).unwrap();
    game.add_bot(None, difficulty).unwrap();
    online(&mut game, &mut host);
    game.start(&mut host, 1).unwrap();
    host.at.insert(A, [0.5, 65.0, -10.5]);
    host.at.insert(BOT, [0.5, 65.0, 10.5]);
    ticks(&mut game, &mut host, 1);
    (game, host)
}

fn life(game: &Game, id: Id) -> Life {
    running_game(game).members[&id].life
}

#[test]
fn without_the_bots_plugin_bot_commands_change_nothing() {
    let (mut game, mut host) = built(1);
    assert_eq!(game.add_bot(Some(1), Difficulty::Easy), Err(NO_BOTS.into()));
    assert_eq!(game.fill_bots(1, Difficulty::Easy), Err(NO_BOTS.into()));
    assert_eq!(game.remove_bots(&mut host, 1), Err(NO_BOTS.into()));
    assert!(game.bot_calls.is_empty() && game.bots.is_empty());
    assert!(game.arena(1).unwrap().queue.is_empty());
}

#[test]
fn filling_requests_one_spawn_per_free_place_and_the_bot_joins_once_online() {
    let (mut game, mut host) = built(1);
    game.bots_present = true;
    game.join(&mut host, who(A), None).unwrap();
    assert_eq!(game.fill_bots(1, Difficulty::Hard), Ok(1));
    let spawned = spawns(&game);
    assert_eq!(spawned.len(), 1);
    let (n, spec) = &spawned[0];
    assert_eq!((*n, spec.name.as_str()), (0, "BOT_Ashwin"));
    assert_eq!(spec.respawn, control::RespawnPolicy::Owner);
    assert_eq!(spec.location.dimension.as_deref(), Some(ARENA));
    assert_eq!(spec.skill, Difficulty::Hard.skill().api());
    assert!(
        game.add_bot(Some(1), Difficulty::Easy).is_err(),
        "a bot on its way holds its place"
    );

    game.bot_spawned(&mut host, 0, Ok(HANDLE));
    ticks(&mut game, &mut host, 1);
    assert_eq!(game.arena(1).unwrap().queue.len(), 1, "not online yet");
    host.online.insert("BOT_Ashwin".into(), BOT);
    ticks(&mut game, &mut host, 1);
    let queue = &game.arena(1).unwrap().queue;
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[1], Who::new(BOT, "BOT_Ashwin"));
    assert!(host.heard_any(A, "BOT_Ashwin has joined (2/2)!"));
    assert!(host.teleports.iter().any(|(to, _)| *to == BOT));

    game.start(&mut host, 1).unwrap();
    assert_eq!(running_game(&game).members[&BOT].team, 1);
    assert!(
        host.slots.iter().any(
            |(to, _, s)| *to == BOT && s.as_deref().is_some_and(|s| s.contains("wooden_sword"))
        ),
        "a bot gets the kit in its real inventory"
    );
}

#[test]
fn a_failed_spawn_frees_the_place_and_a_late_answer_is_removed() {
    let (mut game, mut host) = built(1);
    game.bots_present = true;
    game.add_bot(Some(1), Difficulty::Easy).unwrap();
    game.bot_spawned(&mut host, 0, Err(SpawnFailure::NoProvider));
    assert!(game.bots.is_empty() && !game.bots_present);
    game.bots_present = true;
    game.add_bot(Some(1), Difficulty::Easy).unwrap();
    assert_eq!(game.remove_bots(&mut host, 1), Ok(1));
    game.bot_calls.clear();
    game.bot_spawned(&mut host, 1, Ok(9));
    assert_eq!(game.bot_calls, [BotCall::Remove(9)]);
}

#[test]
fn a_bot_walks_by_bots_api_processes_and_breaks_the_enemy_bed_by_one_break_process() {
    let (mut game, mut host) = against(Difficulty::Hard);
    host.at.remove(&A);
    let sent = walks(&game).len();
    ticks(&mut game, &mut host, 5);
    assert_eq!(walks(&game).len(), sent, "the same walk is not sent again");

    let rush = Difficulty::Hard.skill().rush_after;
    ticks(&mut game, &mut host, rush);
    assert_eq!(
        walks(&game).last().and_then(near),
        Some([0, 65, -8]),
        "off to the red bed"
    );
    host.at.insert(BOT, [0.5, 65.0, -5.5]);
    ticks(&mut game, &mut host, 20);
    let breaks: Vec<_> = walks(&game)
        .into_iter()
        .filter_map(|p| match p {
            control::Process::Break(blocks) => Some(blocks),
            _ => None,
        })
        .collect();
    assert_eq!(breaks.len(), 1, "one break process while the bed stands");
    assert_eq!(
        breaks[0],
        vec![control::BlockPos {
            x: 0,
            y: 65,
            z: -8
        }]
    );
    assert!(running_game(&game).teams[0].bed, "the game no longer breaks for the bot");

    host.set_block(ARENA, [0, 65, -8], "minecraft:air");
    game.broken(
        &mut host,
        Some(&Who::new(BOT, "BOT_Ashwin")),
        ARENA,
        [0, 65, -8],
        "minecraft:red_bed",
    );
    assert!(!running_game(&game).teams[0].bed);
    assert!(host.heard_any(A, "Red Bed was destroyed by BOT_Ashwin"));
}

#[test]
fn a_bot_retries_a_break_process_the_bots_plugin_gave_up() {
    let (mut game, mut host) = against(Difficulty::Hard);
    host.at.remove(&A);
    let rush = Difficulty::Hard.skill().rush_after;
    ticks(&mut game, &mut host, rush);
    host.at.insert(BOT, [0.5, 65.0, -5.5]);
    ticks(&mut game, &mut host, 1);
    let count = |game: &Game| {
        walks(game)
            .iter()
            .filter(|p| matches!(p, control::Process::Break(_)))
            .count()
    };
    assert_eq!(count(&game), 1);
    game.bot_walk_ended(HANDLE);
    ticks(&mut game, &mut host, 2 * SECOND + 1);
    assert_eq!(count(&game), 2, "a failed break is asked again after a pause");
}

#[test]
fn bots_bridge_with_wool_and_may_dig_only_placed_blocks_and_beds() {
    let (game, _) = against(Difficulty::Normal);
    let policy = &spawns(&game)[0].1.policy;
    assert!(policy.allow_place && policy.allow_break);
    assert!(policy.throwaway_items.contains(&"minecraft:red_wool".to_string()));
    assert!(policy.break_allow.contains(&"minecraft:blue_bed".to_string()));
    assert!(policy.break_allow.contains(&"minecraft:end_stone".to_string()));
    assert!(policy.break_allow.iter().all(|b| b.ends_with("_wool")
        || b.ends_with("_bed")
        || b.ends_with("_planks")
        || b.ends_with("end_stone")));
}

#[test]
fn a_bot_dies_respawns_and_is_eliminated_through_the_player_path() {
    let (mut game, mut host) = against(Difficulty::Normal);
    game.died_by(&mut host, BOT, Some(A));
    assert!(matches!(life(&game, BOT), Life::Respawning { .. }));
    assert!(host.heard_any(A, "BOT_Ashwin"));
    let wait = game.settings.respawn + 1;
    ticks(&mut game, &mut host, wait);
    assert_eq!(life(&game, BOT), Life::Alive);

    game.broken(
        &mut host,
        Some(&who(A)),
        ARENA,
        [0, 65, 8],
        "minecraft:blue_bed",
    );
    game.died_by(&mut host, BOT, Some(A));
    assert!(host.heard_any(A, "FINAL KILL!"));
    assert!(host.heard_any(A, "Blue has been eliminated!"));
    game.bot_calls.clear();
    let wait = game.settings.end_delay + 1;
    ticks(&mut game, &mut host, wait);
    assert!(game.bots.is_empty(), "bots go with their game");
    assert!(game.bot_calls.contains(&BotCall::Remove(HANDLE)));
}

#[test]
fn removing_bots_has_them_leave_and_asks_the_bots_plugin_to_remove_them() {
    let (mut game, mut host) = against(Difficulty::Normal);
    game.bot_calls.clear();
    assert_eq!(game.remove_bots(&mut host, 1), Ok(1));
    assert_eq!(game.bot_calls, [BotCall::Remove(HANDLE)]);
    assert!(host.heard_any(A, "BOT_Ashwin left the game."));
    assert!(matches!(game.arena(1).unwrap().phase, Phase::Ending { .. }));
}

#[test]
fn the_bots_plugin_unloading_takes_its_bots_out_like_leaving_players() {
    let (mut game, mut host) = against(Difficulty::Normal);
    game.bot_calls.clear();
    game.bots_unloaded(&mut host);
    assert!(game.bots.is_empty() && !game.bots_present);
    assert!(game.bot_calls.is_empty(), "nobody is left to ask");
    assert!(host.heard_any(A, "BOT_Ashwin left the game."));
    assert!(matches!(game.arena(1).unwrap().phase, Phase::Ending { .. }));
}

#[test]
fn a_removed_event_from_the_bots_plugin_forfeits_the_bot() {
    let (mut game, mut host) = against(Difficulty::Normal);
    game.bot_removed(&mut host, HANDLE + 1);
    assert_eq!(game.bots.len(), 1);
    game.bot_removed(&mut host, HANDLE);
    assert!(game.bots.is_empty());
    assert!(host.heard_any(A, "BOT_Ashwin left the game."));
}

#[test]
fn a_bot_shops_from_its_real_inventory_when_it_picks_something_up() {
    let (mut game, mut host) = against(Difficulty::Easy);
    host.carry = vec![
        (0, stack("minecraft:wooden_sword", 1)),
        (9, stack("minecraft:iron_ingot", 12)),
    ];
    game.bot_picked_up(&mut host, A);
    assert!(host.taken.is_empty(), "players shop by hand");
    game.bot_picked_up(&mut host, BOT);
    assert_eq!(host.taken, [(BOT, "minecraft:iron_ingot".to_owned(), 10)]);
    let given = host
        .gave
        .iter()
        .any(|(to, item, _)| *to == BOT && item.contains("stone_sword"));
    let placed = host
        .slots
        .iter()
        .any(|(to, _, s)| *to == BOT && s.as_deref().is_some_and(|s| s.contains("stone_sword")));
    assert!(given || placed, "{:?} {:?}", host.gave, host.slots);
    game.bot_picked_up(&mut host, BOT);
    assert_eq!(host.taken.len(), 1, "the iron was spent this tick");
}

#[test]
fn bots_save_for_a_sword_then_armor_then_blocks_and_ranged_items() {
    let goal = |carried: &[(u8, &str, u32)]| {
        let carried: Vec<(u8, String)> =
            carried.iter().map(|(s, i, n)| (*s, stack(i, *n))).collect();
        next_buy(&carried).map(|o| o.key)
    };
    assert_eq!(goal(&[]), Some("stone_sword"));
    assert_eq!(goal(&[(0, "minecraft:stone_sword", 1)]), Some("iron_sword"));
    let armed = (0, "minecraft:diamond_sword", 1);
    assert_eq!(
        goal(&[armed, (8, "minecraft:leather_boots", 1)]),
        Some("chainmail")
    );
    assert_eq!(
        goal(&[armed, (8, "minecraft:chainmail_boots", 1)]),
        Some("iron_armor")
    );
    let armored = (8, "minecraft:diamond_boots", 1);
    assert_eq!(goal(&[armed, armored]), Some("wool"));
    let wool = (1, "minecraft:blue_wool", 32);
    assert_eq!(goal(&[armed, armored, wool]), Some("bow"));
    let bow = (2, "minecraft:bow", 1);
    assert_eq!(goal(&[armed, armored, wool, bow]), Some("arrows"));
    let arrows = (3, "minecraft:arrow", 12);
    assert_eq!(goal(&[armed, armored, wool, bow, arrows]), Some("fireball"));
    let charges = (4, "minecraft:fire_charge", 2);
    assert_eq!(
        goal(&[armed, armored, wool, bow, arrows, charges]),
        Some("bridge_egg")
    );
    let eggs = (5, "minecraft:egg", 1);
    assert_eq!(
        goal(&[armed, armored, wool, bow, arrows, charges, eggs]),
        None
    );
}

#[test]
fn a_bot_near_an_enemy_fights_by_one_attack_process_sent_once() {
    let (mut game, mut host) = against(Difficulty::Hard);
    host.at.insert(A, [0.5, 65.0, 8.5]);
    host.at.insert(BOT, [0.5, 65.0, 6.5]);
    ticks(&mut game, &mut host, 40);
    let attacks: Vec<_> = walks(&game)
        .into_iter()
        .filter(|p| matches!(p, control::Process::Attack(_)))
        .collect();
    assert_eq!(attacks.len(), 1, "sent once: {:?}", walks(&game));
    let control::Process::Attack(spec) = &attacks[0] else {
        unreachable!()
    };
    assert_eq!(spec.target, format!("{A:032x}"));
    assert_eq!(spec.until, control::AttackUntil::Death);
}

#[test]
fn a_bot_with_a_bow_shoots_a_far_enemy_and_with_fire_charges_throws_at_mid_range() {
    let (mut game, mut host) = against(Difficulty::Hard);
    game.bots
        .get_mut(&0)
        .unwrap()
        .stock
        .bought("minecraft:bow", 1);
    game.bots
        .get_mut(&0)
        .unwrap()
        .stock
        .bought("minecraft:arrow", 6);
    host.at.insert(A, [0.5, 65.0, -14.5]);
    host.at.insert(BOT, [0.5, 65.0, 0.5]);
    ticks(&mut game, &mut host, 60);
    let shoots: Vec<_> = walks(&game)
        .into_iter()
        .filter_map(|p| match p {
            control::Process::Shoot(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(shoots.len(), 1, "sent once: {:?}", game.bot_calls);
    assert_eq!(shoots[0].target, format!("{A:032x}"));

    let (mut game, mut host) = against(Difficulty::Hard);
    game.bots
        .get_mut(&0)
        .unwrap()
        .stock
        .bought("minecraft:fire_charge", 2);
    host.at.insert(A, [0.5, 65.0, -8.5]);
    host.at.insert(BOT, [0.5, 65.0, 0.5]);
    ticks(&mut game, &mut host, 5);
    let thrown = walks(&game)
        .into_iter()
        .filter(|p| matches!(p, control::Process::UseItem(u) if u.item == "minecraft:fire_charge"))
        .count();
    assert_eq!(thrown, 1, "{:?}", game.bot_calls);
    assert_eq!(game.bots[&0].stock.fireballs, 1);
}

#[test]
fn a_bot_with_a_bridge_egg_throws_it_ahead_over_open_air_once() {
    let (mut game, mut host) = against(Difficulty::Hard);
    game.bots
        .get_mut(&0)
        .unwrap()
        .stock
        .bought("minecraft:egg", 2);
    host.blocks.clear();
    host.at.insert(A, [200.5, 65.0, 200.5]);
    host.at.insert(BOT, [0.5, 65.0, 0.5]);
    ticks(&mut game, &mut host, 5);
    let thrown = walks(&game)
        .into_iter()
        .filter(|p| matches!(p, control::Process::UseItem(u) if u.item == "minecraft:egg"))
        .count();
    assert_eq!(thrown, 1, "{:?}", game.bot_calls);
    assert_eq!(game.bots[&0].stock.eggs, 1);
}
