use super::*;

fn objective(name: &str, criterion: Criterion) -> Objective {
    Objective {
        name: name.into(),
        render_type: criterion.default_render_type(),
        criterion,
        display_name: TextComponent::new(name),
        display_auto_update: false,
        number_format: None,
    }
}

fn packet_ids(packets: &[Bytes]) -> Vec<i32> {
    packets
        .iter()
        .map(|frame| {
            let mut reader = tuff_wire::PacketReader::new(frame.clone());
            let _length = reader.read_var_int().unwrap();
            let _uncompressed = reader.read_var_int().unwrap();
            reader.read_var_int().unwrap()
        })
        .collect()
}

use tuff_vanilla_data::packet::play::Clientbound as Id;

#[tuff_core::watchdog]
#[test]
fn criteria_parse_custom_names_and_statistics() {
    assert_eq!(Criterion::parse("dummy"), Some(Criterion::Dummy));
    assert_eq!(
        Criterion::parse("teamkill.dark_red"),
        Some(Criterion::TeamKill(TeamColor::DarkRed))
    );
    assert_eq!(Criterion::parse("teamkill.reset"), None);
    let mined = Criterion::parse("minecraft.mined:minecraft.stone").unwrap();
    assert_eq!(mined.name(), "minecraft.mined:minecraft.stone");
    assert_eq!(Criterion::parse("mined:stone"), Some(mined));
    assert_eq!(
        Criterion::parse("minecraft.mined:minecraft.diamond_sword"),
        None
    );
    assert_eq!(
        Criterion::parse("minecraft.custom:minecraft.jump")
            .unwrap()
            .name(),
        "minecraft.custom:minecraft.jump"
    );
    assert!(Criterion::Health.read_only());
    assert_eq!(Criterion::Health.default_render_type(), RenderType::Hearts);
    assert_eq!(Criterion::parse("nonsense"), None);
}

#[tuff_core::watchdog]
#[test]
fn display_slots_name_and_number_like_the_game() {
    let slots: Vec<String> = DisplaySlot::all().map(DisplaySlot::name).collect();
    assert_eq!(slots.len(), DisplaySlot::COUNT);
    assert_eq!(slots[0], "list");
    assert_eq!(slots[18], "sidebar.team.white");
    assert_eq!(
        DisplaySlot::from_name("sidebar.team.gold").map(DisplaySlot::id),
        Some(9)
    );
    assert_eq!(DisplaySlot::from_name("sidebar.team.reset"), None);
}

#[tuff_core::watchdog]
#[test]
fn only_displayed_objectives_reach_clients() {
    let mut board = Scoreboard::default();
    assert!(board.add_objective(objective("kills", Criterion::Dummy)));
    assert!(!board.add_objective(objective("kills", Criterion::Dummy)));
    board.set(&Holder::named("alice"), "kills", 3);
    assert!(
        board.take_outbox().is_empty(),
        "untracked objectives are silent"
    );

    board.set_display(DisplaySlot::Sidebar, Some("kills"));
    assert_eq!(
        packet_ids(&board.take_outbox()),
        [
            Id::SetObjective.id(),
            Id::SetDisplayObjective.id(),
            Id::SetScore.id()
        ]
    );
    board.set(&Holder::named("alice"), "kills", 3);
    assert!(
        board.take_outbox().is_empty(),
        "an unchanged score is not resent"
    );
    board.add(&Holder::named("bob"), "kills", 2);
    assert_eq!(packet_ids(&board.take_outbox()), [Id::SetScore.id()]);
    board.reset("bob", "kills");
    assert_eq!(packet_ids(&board.take_outbox()), [Id::ResetScore.id()]);
    assert!(board.score("bob", "kills").is_none());

    board.set_display(DisplaySlot::List, Some("kills"));
    assert_eq!(
        packet_ids(&board.take_outbox()),
        [Id::SetDisplayObjective.id()]
    );
    board.set_display(DisplaySlot::List, None);
    assert_eq!(
        packet_ids(&board.take_outbox()),
        [Id::SetDisplayObjective.id()],
        "still shown elsewhere: only the slot clears"
    );
    board.remove_objective("kills");
    assert_eq!(packet_ids(&board.take_outbox()), [Id::SetObjective.id()]);
    assert!(board.display_objective(DisplaySlot::Sidebar).is_none());
    assert!(board.score("alice", "kills").is_none());
}

#[tuff_core::watchdog]
#[test]
fn new_scores_start_locked_and_a_created_score_is_sent_once_set() {
    let mut board = Scoreboard::default();
    board.add_objective(objective("t", Criterion::Trigger));
    board.set_display(DisplaySlot::Sidebar, Some("t"));
    board.take_outbox();
    assert_eq!(board.get_or_create("p", "t"), 0);
    assert!(board.take_outbox().is_empty());
    assert!(board.score("p", "t").unwrap().locked);
    board.set_locked("p", "t", false);
    assert!(!board.score("p", "t").unwrap().locked);
    board.set(&Holder::named("p"), "t", 0);
    assert!(board.take_outbox().is_empty());
}

#[tuff_core::watchdog]
#[test]
fn teams_track_members_and_send_every_change() {
    let mut board = Scoreboard::default();
    assert!(board.add_team(Team::new("red")));
    assert!(board.add_team(Team::new("blue")));
    assert!(!board.add_team(Team::new("red")));
    assert!(board.join_team("alice", "red"));
    board.take_outbox();
    assert!(
        board.join_team("alice", "red"),
        "rejoining leaves and joins again, as the game does"
    );
    assert_eq!(
        packet_ids(&board.take_outbox()),
        [Id::SetPlayerTeam.id(), Id::SetPlayerTeam.id()]
    );
    assert!(board.join_team("alice", "blue"));
    assert_eq!(board.team_of("alice").unwrap().name, "blue");
    assert!(board.team("red").unwrap().members.is_empty());
    board.take_outbox();
    board.modify_team("blue", |team| team.color = Some(TeamColor::Blue));
    assert_eq!(packet_ids(&board.take_outbox()), [Id::SetPlayerTeam.id()]);
    assert!(board.leave_team("alice"));
    assert!(!board.leave_team("alice"));
    board.join_team("bob", "blue");
    board.remove_team("blue");
    assert!(board.team_of("bob").is_none());
    let formatted = board.team("red").unwrap().formatted_display_name();
    assert_eq!(formatted.translate.as_deref(), Some("chat.square_brackets"));
}

#[tuff_core::watchdog]
#[test]
fn the_saved_form_round_trips_with_the_games_keys() {
    let mut board = Scoreboard::default();
    let mut deaths = objective("deaths", Criterion::DeathCount);
    deaths.display_name = TextComponent::new("Deaths").color("red");
    deaths.number_format = Some(NumberFormat::Fixed(TextComponent::new("-")));
    board.add_objective(deaths);
    board.add_objective(objective("hp", Criterion::Health));
    board.set(&Holder::named("alice"), "deaths", 4);
    board.set_locked("alice", "hp", false);
    board.set_display(DisplaySlot::Team(TeamColor::Gold), Some("deaths"));
    let mut team = Team::new("gold");
    team.color = Some(TeamColor::Gold);
    team.friendly_fire = false;
    team.collision_rule = CollisionRule::PushOwnTeam;
    board.add_team(team);
    board.join_team("alice", "gold");
    let saved = board.save();
    assert!(!board.is_dirty());

    let objectives = saved.get_list("Objectives").unwrap();
    let first = objectives[0].as_compound().unwrap();
    assert_eq!(first.get_str("Name"), Some("deaths"));
    assert_eq!(first.get_str("CriteriaName"), Some("deathCount"));
    assert_eq!(first.get_str("RenderType"), None, "defaults are left out");
    assert_eq!(
        objectives[1].as_compound().unwrap().get_str("RenderType"),
        Some("hearts")
    );
    let score = saved.get_list("PlayerScores").unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(score.get_str("Name"), Some("alice"));
    assert_eq!(score.get_int("Score"), Some(4));
    assert_eq!(
        score.get_bool("Locked"),
        Some(true),
        "new scores start locked"
    );
    assert_eq!(
        saved
            .get_compound("DisplaySlots")
            .unwrap()
            .get_str("sidebar.team.gold"),
        Some("deaths")
    );
    let team = saved.get_list("Teams").unwrap()[0].as_compound().unwrap();
    assert_eq!(team.get_str("TeamColor"), Some("gold"));
    assert_eq!(team.get_bool("AllowFriendlyFire"), Some(false));
    assert_eq!(team.get_str("CollisionRule"), Some("pushOwnTeam"));

    let loaded = Scoreboard::load(&saved);
    assert_eq!(
        loaded.objective("deaths"),
        board.objective("deaths"),
        "objective fields survive"
    );
    assert_eq!(loaded.score("alice", "deaths").unwrap().value, 4);
    assert!(!loaded.score("alice", "hp").unwrap().locked);
    assert_eq!(
        loaded
            .display_objective(DisplaySlot::Team(TeamColor::Gold))
            .map(|o| o.name.as_str()),
        Some("deaths")
    );
    assert_eq!(loaded.team_of("alice").unwrap().name, "gold");
    assert_eq!(loaded.team("gold"), board.team("gold"));
    let ids = packet_ids(&loaded.join_packets());
    assert_eq!(ids[0], Id::SetPlayerTeam.id());
    assert_eq!(ids[1], Id::SetObjective.id());
}

#[tuff_core::watchdog]
#[test]
fn loading_skips_scores_of_unknown_objectives_and_bad_criteria() {
    let data = Compound::new()
        .with(
            "Objectives",
            Tag::List(vec![
                Tag::Compound(
                    Compound::new()
                        .with("Name", "bad")
                        .with("CriteriaName", "nope")
                        .with("DisplayName", "bad"),
                ),
                Tag::Compound(Compound::new().with("Name", "ok").with("DisplayName", "ok")),
            ]),
        )
        .with(
            "PlayerScores",
            Tag::List(vec![Tag::Compound(
                Compound::new()
                    .with("Name", "x")
                    .with("Objective", "bad")
                    .with("Score", 1),
            )]),
        );
    let board = Scoreboard::load(&data);
    assert!(board.objective("bad").is_none());
    assert_eq!(board.objective("ok").unwrap().criterion, Criterion::Dummy);
    assert_eq!(board.holders().count(), 0);
}

#[tuff_core::watchdog]
#[test]
fn kills_count_for_the_killer_and_team_colors_for_both_sides() {
    let mut board = Scoreboard::default();
    for (name, criterion) in [
        ("all", Criterion::TotalKillCount),
        ("players", Criterion::PlayerKillCount),
        ("killed_red", Criterion::TeamKill(TeamColor::Red)),
        ("by_blue", Criterion::KilledByTeam(TeamColor::Blue)),
    ] {
        board.add_objective(objective(name, criterion));
    }
    let mut red = Team::new("red");
    red.color = Some(TeamColor::Red);
    board.add_team(red);
    let mut blue = Team::new("blue");
    blue.color = Some(TeamColor::Blue);
    board.add_team(blue);
    board.join_team("alice", "blue");
    board.join_team("bob", "red");
    let alice = Holder::named("alice");
    board.award_kill(&alice, &Holder::named("bob"), true);
    board.award_kill(&alice, &Holder::named("some-mob"), false);
    board.award_kill(&alice, &alice, true);
    assert_eq!(board.score("alice", "all").unwrap().value, 2);
    assert_eq!(board.score("alice", "players").unwrap().value, 1);
    assert_eq!(board.score("alice", "killed_red").unwrap().value, 1);
    assert_eq!(board.score("bob", "by_blue").unwrap().value, 1);
    assert_eq!(board.score("some-mob", "by_blue").unwrap().value, 1);
    assert!(board.criteria().contains(&Criterion::TotalKillCount));
}

#[tuff_core::watchdog]
#[test]
fn loads_and_rewrites_a_scoreboard_vanilla_saved() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/vanilla-26.3/scoreboard.dat"
    );
    let root = tuff_storage::files::read_nbt_file(std::path::Path::new(path))
        .unwrap()
        .unwrap();
    let data = tuff_storage::document::inner(Some(root));
    let mut board = Scoreboard::load(&data);

    let kills = board.objective("kills").unwrap();
    assert_eq!(kills.display_name, TextComponent::new("Kills"));
    assert!(kills.display_auto_update);
    assert_eq!(
        kills.number_format,
        Some(NumberFormat::Fixed(TextComponent::new("-")))
    );
    let hp = board.objective("hp").unwrap();
    assert_eq!(hp.criterion, Criterion::Health);
    assert_eq!(hp.render_type, RenderType::Hearts);
    assert_eq!(hp.display_name, TextComponent::new("Health").color("red"));
    assert_eq!(
        board.objective("mined").unwrap().criterion.name(),
        "minecraft.mined:minecraft.stone"
    );
    assert_eq!(board.score("alice", "kills").unwrap().value, 7);
    assert_eq!(
        board.score("alice", "kills").unwrap().display,
        Some(TextComponent::new("Al"))
    );
    assert_eq!(board.score("#hidden", "kills").unwrap().value, -3);
    assert!(!board.score("bob", "vote").unwrap().locked);
    assert_eq!(
        board
            .display_objective(DisplaySlot::Sidebar)
            .map(|o| o.name.as_str()),
        Some("kills")
    );
    assert_eq!(
        board
            .display_objective(DisplaySlot::List)
            .map(|o| o.name.as_str()),
        Some("hp")
    );
    assert_eq!(
        board
            .display_objective(DisplaySlot::Team(TeamColor::Red))
            .map(|o| o.name.as_str()),
        Some("kills")
    );
    let red = board.team("red").unwrap();
    assert_eq!(red.display_name, TextComponent::new("Red Team"));
    assert_eq!(red.color, Some(TeamColor::Red));
    assert!(!red.friendly_fire);
    assert!(red.see_friendly_invisibles);
    assert_eq!(red.collision_rule, CollisionRule::PushOwnTeam);
    assert_eq!(red.prefix, TextComponent::new("[R] "));
    assert_eq!(board.team_of("bob").unwrap().name, "red");
    assert!(board.team("empty").unwrap().members.is_empty());

    let rewritten = board.save();
    for list in ["Objectives", "PlayerScores", "Teams"] {
        let read = data.get_list(list).unwrap();
        let written = rewritten.get_list(list).unwrap();
        assert_eq!(read.len(), written.len(), "{list}");
        for entry in read {
            assert!(
                written.contains(entry),
                "{list} entry {entry:?} is written back as read"
            );
        }
    }
    assert_eq!(
        data.get_compound("DisplaySlots"),
        rewritten.get_compound("DisplaySlots")
    );
}

fn tag(prefix: &str) -> Nametag {
    Nametag {
        prefix: TextComponent::new(prefix),
        suffix: TextComponent::default(),
        color: Some(TeamColor::Red),
    }
}

fn team_packet(name: &str, action: TeamAction) -> Bytes {
    framed(&SetPlayerTeam { name, action })
}

#[tuff_core::watchdog]
#[test]
fn a_role_player_gets_a_synthetic_team_and_viewers_joining_see_it() {
    let mut board = Scoreboard::default();
    board.set_nametag("Alex", Some(tag("[A] ")));
    let name = nametag_team_name("Alex");
    assert!(name.len() <= 16);
    let outbox = board.take_outbox();
    assert_eq!(outbox.len(), 1);
    let team = board.nametag_team("Alex").unwrap();
    assert_eq!(outbox[0], team_packet(&name, TeamAction::Add(&team)));
    assert_eq!(board.join_packets(), outbox);
    assert!(board.teams().next().is_none());
    assert!(board.take_teams().is_none());

    board.set_nametag("Alex", Some(tag("[B] ")));
    let team = board.nametag_team("Alex").unwrap();
    assert_eq!(
        board.take_outbox(),
        vec![team_packet(&name, TeamAction::Change(&team))]
    );
    board.set_nametag("Alex", Some(tag("[B] ")));
    assert!(board.take_outbox().is_empty());

    board.set_nametag("Alex", None);
    assert_eq!(
        board.take_outbox(),
        vec![team_packet(&name, TeamAction::Remove)]
    );
    assert!(board.join_packets().is_empty());
}

#[tuff_core::watchdog]
#[test]
fn a_vanilla_team_keeps_its_packets_and_wins_over_the_role_nametag() {
    let mut board = Scoreboard::default();
    board.add_team(Team::new("red"));
    board.join_team("Alex", "red");
    board.take_outbox();
    board.set_nametag("Alex", Some(tag("[A] ")));
    assert!(board.take_outbox().is_empty());
    assert_eq!(board.join_packets().len(), 1);
    board.set_nametag("Alex", Some(tag("[B] ")));
    assert!(board.take_outbox().is_empty());

    board.leave_team("Alex");
    let name = nametag_team_name("Alex");
    let team = board.nametag_team("Alex").unwrap();
    assert_eq!(
        board.take_outbox(),
        vec![
            team_packet("red", TeamAction::Leave(&["Alex".to_owned()])),
            team_packet(&name, TeamAction::Add(&team)),
        ]
    );

    board.join_team("Alex", "red");
    assert_eq!(
        board.take_outbox(),
        vec![
            team_packet(&name, TeamAction::Remove),
            team_packet("red", TeamAction::Join(&["Alex".to_owned()])),
        ]
    );
    board.remove_team("red");
    let team = board.nametag_team("Alex").unwrap();
    assert_eq!(
        board.take_outbox(),
        vec![
            team_packet("red", TeamAction::Remove),
            team_packet(&name, TeamAction::Add(&team)),
        ]
    );
}
