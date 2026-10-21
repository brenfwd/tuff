use crate::common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::protocol::play::{Movement, Serverbound};
use tuff_replay::script::Event;
use tuff_replay::viewer::Viewer;
use tuff_vanilla_data::packet::play::Clientbound;

const MAX: usize = tuff_replay::MAX_RECORDING_BYTES;

fn packet_id(payload: &bytes::Bytes) -> i32 {
    tuff_wire::PacketReader::new(payload.clone())
        .read_var_int()
        .unwrap()
}

/// Records a short session of `Tester` joining and walking on a real server.
async fn record() -> (PathBuf, TempDir) {
    let instance = TempDir::new();
    let traces = TempDir::new();
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .trace_dir(traces.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    handle
        .run_command("tuff tracing Tester start")
        .await
        .unwrap();
    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new("Tester")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks >= 9).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    for command in [
        "scoreboard objectives add kills dummy",
        "scoreboard objectives setdisplay sidebar kills",
        "team add red",
        "title @a title \"Hi\"",
        "worldborder set 500",
    ] {
        let reply = handle.run_command(command).await.unwrap();
        let reply: String = reply.iter().map(|t| t.plain()).collect();
        assert!(!reply.contains("No player"), "{command}: {reply}");
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let start = bot.state().position;
    for step in 1..=5 {
        bot.send(Serverbound::Move(Movement {
            position: Some([start[0] + 0.2 * f64::from(step), start[1], start[2]]),
            rotation: Some((90.0, 0.0)),
            on_ground: true,
            horizontal_collision: false,
        }));
        bot.send(Serverbound::ClientTickEnd);
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    drop(bot);
    let stopped = handle
        .run_command("tuff tracing Tester stop")
        .await
        .unwrap();
    let path = stopped.iter().map(|t| t.plain()).collect::<String>();
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    drop(instance);
    let file = std::fs::read_dir(traces.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(path.contains("Stopped tracing"), "{path}");
    (file, traces)
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_recorded_session_replays_to_a_spectating_bot() {
    let (file, _traces) = record().await;
    let script = Arc::new(tuff_replay::script::load(&file, MAX).unwrap());
    assert_eq!(script.player, "Tester");
    assert_eq!(script.uuid, tuff_core::auth::offline_uuid("Tester"));
    assert!(!script.config.is_empty());
    assert!(
        script
            .events
            .iter()
            .any(|e| matches!(e.event, Event::Moved { position: Some(p), .. } if p[0] != 0.0))
    );

    let (addrs, task) = tuff_replay::start(&["127.0.0.1:0".into()], script.clone())
        .await
        .unwrap();
    let viewer = Bot::connect(
        addrs[0],
        BotConfig {
            view_distance: 2,
            ..BotConfig::new("Watcher")
        },
    )
    .await
    .unwrap();
    viewer.wait_for(|state| state.chunks >= 9).await.unwrap();
    assert_eq!(viewer.state().entity_id, i32::MAX);
    viewer
        .wait_for(|state| {
            state
                .entities
                .values()
                .any(|seen| seen.kind == Some(tuff_vanilla_data::registry::EntityType::Player))
        })
        .await
        .unwrap();

    let second = Bot::connect(addrs[0], BotConfig::new("Second"))
        .await
        .unwrap();
    second.wait_for(|state| state.chunks > 0).await.unwrap();
    assert_eq!(second.state().entity_id, i32::MAX - 1);
    task.abort();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn seeking_back_matches_straight_playback_and_pause_holds() {
    let (file, _traces) = record().await;
    let script = Arc::new(tuff_replay::script::load(&file, MAX).unwrap());
    let end = script.duration();
    let mut out = Vec::new();

    let mut straight = Viewer::new(script.clone(), 7);
    straight.step(Duration::from_nanos(end as u64 / 2), &mut out);
    let half = straight.transform.clone();
    straight.seek(end, &mut out);
    assert_eq!(straight.cursor(), script.events.len());
    let full = straight.transform.clone();
    assert!(!full.chunks.is_empty());

    let mut seeker = Viewer::new(script.clone(), 7);
    seeker.seek(end, &mut out);
    out.clear();
    seeker.command(&format!("replay seek {}", end as f64 / 2e9), &mut out);
    assert!(!out.is_empty(), "a backward seek clears and replays");
    assert_eq!(seeker.transform.chunks, half.chunks);
    assert_eq!(seeker.transform.entities, half.entities);
    assert_eq!(seeker.transform.position, half.position);
    seeker.command("replay pause", &mut out);
    let held = seeker.cursor();
    seeker.step(Duration::from_secs(3600), &mut out);
    assert_eq!(seeker.cursor(), held);
    seeker.command("replay play", &mut out);
    seeker.step(Duration::from_secs(3600), &mut out);
    assert_eq!(seeker.transform.chunks, full.chunks);
    assert_eq!(seeker.transform.entities, full.entities);
    assert_eq!(seeker.transform.position, full.position);
    assert!(seeker.status().starts_with("Playing"));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn seeking_back_resets_tab_list_scoreboard_titles_and_border() {
    let (file, _traces) = record().await;
    let script = Arc::new(tuff_replay::script::load(&file, MAX).unwrap());
    let objective = script
        .events
        .iter()
        .position(|timed| {
            matches!(&timed.event, Event::Packet(p) if packet_id(p) == Clientbound::SetObjective.id())
        })
        .expect("the objective was recorded");
    let before = script.events.read_from(objective)[0].ns - 1;
    let mut out = Vec::new();

    let mut straight = Viewer::new(script.clone(), 7);
    straight.step(Duration::from_nanos(before as u64), &mut out);
    let earlier = straight.transform.clone();
    assert!(earlier.objectives.is_empty() && earlier.teams.is_empty());
    assert!(earlier.tab_list.contains(&script.uuid));
    straight.step(Duration::from_secs(3600), &mut out);
    let full = straight.transform.clone();
    assert!(full.objectives.contains("kills"), "{:?}", full.objectives);
    assert!(full.teams.contains("red"), "{:?}", full.teams);
    assert!(full.titles, "titles");
    assert!(full.border, "border");

    let mut seeker = Viewer::new(script.clone(), 7);
    seeker.seek(script.duration(), &mut out);
    assert_eq!(seeker.transform, full);
    out.clear();
    seeker.seek(before, &mut out);
    let sent: Vec<i32> = out.iter().map(packet_id).collect();
    for kind in [
        Clientbound::PlayerInfoRemove,
        Clientbound::SetObjective,
        Clientbound::SetPlayerTeam,
        Clientbound::ClearTitles,
        Clientbound::InitializeBorder,
    ] {
        assert!(sent.contains(&kind.id()), "{kind:?} not sent on reset");
    }
    let reset = seeker.transform.clone();
    assert_eq!(reset.tab_list, earlier.tab_list);
    assert_eq!(reset.objectives, earlier.objectives);
    assert_eq!(reset.teams, earlier.teams);
    assert_eq!(reset.titles, earlier.titles);
    assert_eq!(reset.chunks, earlier.chunks);
    assert_eq!(reset.entities, earlier.entities);
}

#[tuff_core::watchdog]
#[test]
fn unusable_recordings_are_refused() {
    let dir = TempDir::new();
    let garbage = dir.path().join("garbage.tuffrec");
    std::fs::write(&garbage, b"definitely not a recording").unwrap();
    let err = tuff_replay::script::load(&garbage, MAX).unwrap_err();
    assert!(
        format!("{err:#}").contains("not a tuff recording"),
        "{err:#}"
    );

    let empty = dir.path().join("empty.tuffrec");
    let header = tuff_trace::format::Header::current("Nobody", 0);
    let writer =
        tuff_trace::format::Writer::new(std::fs::File::create(&empty).unwrap(), &header).unwrap();
    writer.finish().unwrap();
    let err = tuff_replay::script::load(&empty, MAX).unwrap_err();
    assert!(format!("{err:#}").contains("configuration"), "{err:#}");

    let other = dir.path().join("other.tuffrec");
    let mut header = tuff_trace::format::Header::current("Nobody", 0);
    header.protocol_version += 1;
    let writer =
        tuff_trace::format::Writer::new(std::fs::File::create(&other).unwrap(), &header).unwrap();
    writer.finish().unwrap();
    let err = tuff_replay::script::load(&other, MAX).unwrap_err();
    assert!(format!("{err:#}").contains("protocol"), "{err:#}");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_walking_bot_stays_online() {
    let instance = TempDir::new();
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new("Tester")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks >= 9).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let start = bot.state().position;
    for step in 1..=5 {
        bot.send(Serverbound::Move(Movement {
            position: Some([start[0] + 0.2 * f64::from(step), start[1], start[2]]),
            rotation: Some((90.0, 0.0)),
            on_ground: true,
            horizontal_collision: false,
        }));
        bot.send(Serverbound::ClientTickEnd);
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    let reply = handle.run_command("list").await.unwrap();
    let reply: String = reply.iter().map(|t| t.plain()).collect();
    assert!(reply.contains("1 of"), "{reply}");
    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
