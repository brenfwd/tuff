//! `/tuff timings` on a running server: the report lands in the
//! instance's `timings/` directory with tick, network, session and region
//! spans, and exports to folded stacks and speedscope.

use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_timings::report::{Report, ReportNode};

async fn say(handle: &tuff::ServerHandle, command: &str) -> Vec<String> {
    handle
        .run_command(command)
        .await
        .unwrap()
        .iter()
        .map(|t| t.plain())
        .collect()
}

fn find<'a>(nodes: &'a [ReportNode], name: &str) -> Vec<&'a ReportNode> {
    let mut out = Vec::new();
    for node in nodes {
        if node.name == name {
            out.push(node);
        }
        out.extend(find(&node.children, name));
    }
    out
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_recording_writes_a_report_with_ticks_network_and_regions() {
    let _serial = tuff_timings::serial_async().await;
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
    let started = say(&handle, "tuff timings record").await;
    assert!(started[0].starts_with("Recording timings"), "{started:?}");
    let mut ticks = handle.tick_times().await.unwrap();
    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new("Timer")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    let mut seen = 0;
    while seen < 20 {
        if ticks.recv().await.is_ok() {
            seen += 1;
        }
    }
    let stopped = say(&handle, "tuff timings stop").await;
    eprintln!("{}", stopped.join("\n"));
    let saved = stopped[1].strip_prefix("Saved ").expect("saved").to_owned();
    let file = saved.split(' ').next().unwrap().to_owned();
    assert!(
        file.starts_with("timings/timings-") && file.ends_with(".json"),
        "{file}"
    );
    assert!(stopped[0].contains(" TPS, mspt mean "), "{stopped:?}");

    let bytes = std::fs::read(instance.path().join(&file)).unwrap();
    let report = Report::from_json(&bytes).unwrap();
    let ticks = report.ticks.as_ref().expect("tick summary");
    assert!(ticks.count >= 20 && ticks.tps > 0.0, "{ticks:?}");
    assert!(ticks.mspt.max >= ticks.mspt.mean);
    for name in [
        "coordinator.tick",
        "server.tick",
        "region.tick",
        "net.encode",
        "net.decode",
        "session.tick",
        "region.job",
    ] {
        assert!(!find(&report.merged, name).is_empty(), "no {name} span");
    }
    let encoded: Vec<&str> = find(&report.merged, "net.encode")
        .iter()
        .filter_map(|n| n.tag.as_deref())
        .collect();
    assert!(encoded.contains(&"LevelChunkWithLight"), "{encoded:?}");
    assert!(
        find(&report.merged, "region.tick")
            .iter()
            .any(|n| n.tag.as_deref() == Some("minecraft:overworld 0,0"))
    );
    assert!(report.threads.len() > 1);

    let id = file
        .trim_start_matches("timings/")
        .trim_end_matches(".json")
        .to_owned();
    assert!(say(&handle, "tuff timings list").await[0].contains(&id));
    let exported = say(&handle, &format!("tuff timings export {id} folded")).await;
    assert_eq!(exported, [format!("Wrote timings/{id}.folded")]);
    let folded =
        std::fs::read_to_string(instance.path().join(format!("timings/{id}.folded"))).unwrap();
    assert!(
        folded
            .lines()
            .all(|l| l.rsplit(' ').next().unwrap().parse::<u64>().is_ok())
    );
    say(&handle, &format!("tuff timings export {id} speedscope")).await;
    let scope: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            instance
                .path()
                .join(format!("timings/{id}.speedscope.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(scope["profiles"][0]["unit"], "nanoseconds");
    assert_eq!(say(&handle, "tuff timings list").await.len(), 1);
    assert_eq!(
        say(&handle, "tuff timings export nope folded").await,
        ["no timings report named \"nope\""]
    );

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
