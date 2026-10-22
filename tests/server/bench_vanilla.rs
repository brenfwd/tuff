use crate::common;
use crate::console;
use crate::servers;
use crate::vanilla;

use std::net::{IpAddr, Ipv4Addr};

use servers::{Choice, Kind};
use vanilla::{Settings, VanillaOptions};

#[tuff_core::watchdog]
#[test]
fn tick_query_reads_both_states() {
    let normal = "The game is running normallyTarget tick rate: 20.0 per second.\n\
                  Average time per tick: 0.2ms (Target: 50.0ms)Percentiles: P50: 0.2ms \
                  P95: 0.4ms P99: 0.6ms. Sample: 100";
    let query = console::tick_query(normal).unwrap();
    assert!(!query.sprinting);
    assert_eq!(
        (query.mean_ms, query.p50_ms, query.p95_ms, query.p99_ms),
        (0.2, 0.2, 0.4, 0.6)
    );
    assert_eq!(query.sample, 100);
    let sprinting = "The game is sprinting\nTarget tick rate: 20.0 per second (ignored, \
                     reference only).\nAverage time per tick: 12.5ms\nPercentiles: P50: 10.0ms \
                     P95: 20.25ms P99: 31.0ms. Sample: 87";
    let query = console::tick_query(sprinting).unwrap();
    assert!(query.sprinting && console::sprinting(sprinting));
    assert_eq!(
        (query.mean_ms, query.p99_ms, query.sample),
        (12.5, 31.0, 87)
    );
    assert!(console::tick_query("The game is running normally").is_none());
}

#[tuff_core::watchdog]
#[test]
fn command_answers_parse() {
    let locate = "The nearest #minecraft:village (minecraft:village_plains) is at \
                  [640, ~, -816] (812 blocks away)";
    assert_eq!(console::located(locate), Some((640, -816)));
    assert_eq!(console::located("Could not find a structure"), None);
    assert_eq!(console::entity_count("Test passed. Count: 42"), Some(42));
    assert_eq!(console::entity_count("Test failed"), Some(0));
    assert_eq!(console::entity_count("Test passed"), Some(1));
    assert_eq!(console::entity_count("Unknown command"), None);
    assert_eq!(
        console::player_list("There are 2 of a max of 20 players online: Bench, Fly0"),
        Some(vec!["Bench".to_string(), "Fly0".to_string()])
    );
    assert_eq!(
        console::player_list("There are 0 of a max of 20 players online: "),
        Some(Vec::new())
    );
    assert!(console::player_missing("No player was found"));
    assert!(!console::player_missing(
        "Set Fly0's game mode to Spectator Mode"
    ));
    assert!(console::opped("Made Bench a server operator"));
    assert!(console::opped(
        "Nothing changed. The player already is an operator"
    ));
    assert!(!console::opped("Unknown or incomplete command"));
}

#[tuff_core::watchdog]
#[test]
fn chunk_rows_count_holders_and_full_chunks() {
    let mut rows = console::ChunkRows::default();
    rows.add("x,z,level,in_memory,status,full_status,spawning\n");
    rows.add(
        "x,z,level,in_memory,status,full_status,spawning\n\
         0,0,33,true,full,ENTITY_TICKING,true\n\
         1,0,34,true,full,FULL,false\n\
         2,0,35,true,features,INACCESSIBLE,false\n",
    );
    assert_eq!(
        rows,
        console::ChunkRows {
            holders: 3,
            full: 2
        }
    );
}

#[tuff_core::watchdog]
#[test]
fn properties_are_offline_on_the_given_ports() {
    let mut settings = Settings {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 40001,
        rcon_port: 40002,
        rcon_password: "secret".into(),
        seed: -7,
        view_distance: 12,
        simulation_distance: 9,
        max_players: 30,
    };
    let text = vanilla::properties(&settings);
    for line in [
        "online-mode=false",
        "server-ip=127.0.0.1",
        "server-port=40001",
        "enable-rcon=true",
        "rcon.port=40002",
        "rcon.password=secret",
        "level-seed=-7",
        "view-distance=12",
        "simulation-distance=9",
        "max-tick-time=-1",
        "max-players=30",
        "spawn-protection=0",
    ] {
        assert!(
            text.lines().any(|l| l == line),
            "{line} missing from\n{text}"
        );
    }
    assert!(!text.contains("25565"));
    settings.host = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    assert!(
        vanilla::properties(&settings)
            .lines()
            .any(|l| l == "server-ip=")
    );
}

#[tuff_core::watchdog]
#[test]
fn eula_needs_the_flag_or_the_env() {
    let mut options = VanillaOptions::default();
    assert!(!options.accepts(None));
    assert!(!options.accepts(Some("0")));
    assert!(options.accepts(Some("1")));
    assert!(options.accepts(Some("TRUE")));
    let next = || -> anyhow::Result<String> { panic!("takes no value") };
    assert!(options.flag("--accept-mojang-eula", next).unwrap());
    assert!(options.accepts(None));
    assert!(
        options
            .flag("--java_opts", || Ok("-Xmx2G -Xms1G".into()))
            .unwrap()
    );
    assert_eq!(options.java_opts, ["-Xmx2G", "-Xms1G"]);
    assert!(!options.flag("--seed", || Ok("1".into())).unwrap());
}

#[tuff_core::watchdog]
#[test]
fn rcon_packets_round_trip() {
    let packet = vanilla::encode_rcon(7, 2, "tick query");
    assert_eq!(
        i32::from_le_bytes(packet[..4].try_into().unwrap()) as usize,
        packet.len() - 4
    );
    let (id, kind, body) = vanilla::decode_rcon(&packet[4..]).unwrap();
    assert_eq!((id, kind, body.as_str()), (7, 2, "tick query"));
    assert!(vanilla::decode_rcon(&[0; 9]).is_err());
}

#[tuff_core::watchdog]
#[test]
fn server_choice_and_both_arguments() {
    assert_eq!(
        Choice::from_name("vanilla"),
        Some(Choice::One(Kind::Vanilla))
    );
    assert_eq!(Choice::from_name("both"), Some(Choice::Both));
    assert_eq!(Choice::from_name("paper"), None);
    assert!(!Choice::One(Kind::Tuff).needs_vanilla());
    assert!(Choice::Both.needs_vanilla());
    let args = [
        "--bench",
        "--server",
        "both",
        "--players",
        "2",
        "--out",
        "a.json",
    ];
    assert_eq!(
        servers::args_without_server(args.map(String::from)),
        ["--bench", "--players", "2"]
    );
}

const CRASH_REPORT: &str = "---- Minecraft Crash Report ----
// a comment line

Time: 2026-10-03 01:36:12
Description: Watching Server

java.lang.Error: Watchdog (Watching Server)
\tat io.netty.Native.wakeup(Native Method)
\tat net.minecraft.network.Link.send(Link.java:297)
\tat net.minecraft.server.Session.teleport(Session.java:1417)
\tat net.minecraft.server.Session.handleMove(Session.java:1235)
\tat net.minecraft.server.Session.handleAccept(Session.java:604)


A detailed walkthrough of the error follows:
\tat net.minecraft.server.Other.frame(Other.java:1)
";

const WATCHDOG_LOG: &str =
    "[01:35:10] [Server thread/WARN]: Can't keep up! Is the server overloaded?
[01:36:11] [Server Watchdog/ERROR]: A single server tick took 60.00 seconds (should be max 0.05)
[01:36:11] [Server Watchdog/ERROR]: Considering it to be crashed, server will forcibly shutdown.
[01:36:12] [Server thread/WARN]: Fly3 moved too quickly! 1.0,0.3,14.9
";

#[tuff_core::watchdog]
#[test]
fn crash_reports_and_log_errors_name_the_cause() {
    let report = vanilla::parse_crash_report(CRASH_REPORT).unwrap();
    assert_eq!(report.description, "Watching Server");
    assert_eq!(report.error, "java.lang.Error: Watchdog (Watching Server)");
    assert_eq!(
        report.frames,
        ["Link.send", "Session.teleport", "Session.handleMove"]
    );
    assert!(vanilla::parse_crash_report("no description here").is_none());

    let errors: Vec<String> = WATCHDOG_LOG
        .lines()
        .filter_map(vanilla::log_error)
        .map(String::from)
        .collect();
    assert_eq!(errors.len(), 2);
    assert_eq!(
        vanilla::crash_cause(Some(&report), &errors).unwrap(),
        "watchdog: A single server tick took 60.00 seconds (should be max 0.05); \
         at Link.send < Session.teleport < Session.handleMove"
    );
    assert_eq!(
        vanilla::crash_cause(Some(&report), &[]).unwrap(),
        "watchdog: Watching Server: java.lang.Error: Watchdog (Watching Server); \
         at Link.send < Session.teleport < Session.handleMove"
    );
    assert_eq!(
        vanilla::crash_cause(None, &["Failed to bind".into()]).unwrap(),
        "error: Failed to bind"
    );
    assert!(vanilla::crash_cause(None, &[]).is_none());
}

#[tuff_core::watchdog]
#[test]
fn a_dead_instance_is_diagnosed_from_its_files() {
    let instance = common::TempDir::new();
    let dir = instance.path();
    assert_eq!(vanilla::diagnose(dir), None);
    std::fs::create_dir_all(dir.join("crash-reports")).unwrap();
    std::fs::create_dir_all(dir.join("logs")).unwrap();
    std::fs::write(
        dir.join("crash-reports/crash-2026-10-03_01.00.00-server.txt"),
        "Description: Older\n\njava.lang.Error: old\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("crash-reports/crash-2026-10-03_01.36.12-server.txt"),
        CRASH_REPORT,
    )
    .unwrap();
    let flood =
        "[01:35:00] [Server thread/WARN]: Fly0 moved too quickly! 0.0,0.3,15.0\n".repeat(2000);
    std::fs::write(dir.join("logs/latest.log"), flood + WATCHDOG_LOG).unwrap();
    let cause = vanilla::diagnose(dir).unwrap();
    assert!(
        cause.starts_with("watchdog: A single server tick took 60.00 seconds"),
        "{cause}"
    );
    assert!(cause.ends_with("Session.handleMove"), "{cause}");
    assert_eq!(servers::diagnose(Kind::Tuff, dir), None);
    let failed = servers::failure(
        Kind::Vanilla,
        dir,
        &anyhow::anyhow!("RCON connection closed"),
    );
    assert_eq!(failed["error"], "RCON connection closed");
    assert_eq!(failed["cause"].as_str(), Some(cause.as_str()));
}

#[tuff_core::watchdog]
#[test]
fn both_summary_keeps_the_survivor_and_flags_bad_runs() {
    let tuff = serde_json::json!({
        "server": "tuff",
        "ticks": {"p50": 2.0},
        "bots": {"moves": 1000, "setbacks": 0},
    });
    let vanilla = serde_json::json!({
        "server": "vanilla",
        "failed": {"error": "rcon closed", "cause": "watchdog: A single server tick took 60.00 seconds"},
    });
    let summary = servers::both_summary(&tuff, &vanilla);
    let mut lines = summary.lines();
    assert_eq!(
        lines.next(),
        Some("tuff     valid: 0 setbacks in 1000 moves (0.00%)")
    );
    assert_eq!(
        lines.next(),
        Some("vanilla  FAILED: watchdog: A single server tick took 60.00 seconds")
    );
    assert!(
        summary
            .lines()
            .any(|l| l.starts_with("ticks.p50") && l.contains("n/a"))
    );
    let flooded = serde_json::json!({"bots": {"moves": 1000, "setbacks": 500}});
    assert!(
        servers::both_summary(&tuff, &flooded)
            .lines()
            .nth(1)
            .unwrap()
            .starts_with("vanilla  INVALID: 500 setbacks in 1000 moves")
    );
    assert_eq!(summary.lines().nth(2), Some("workload comparable"));
    let held = serde_json::json!({"bots": {"moves": 760, "setbacks": 0}});
    let summary = servers::both_summary(&tuff, &held);
    assert!(
        summary
            .lines()
            .nth(2)
            .unwrap()
            .starts_with("workload INVALID: workload mismatch, vanilla made 760 moves"),
        "{summary}"
    );
}
