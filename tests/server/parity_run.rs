//! Tuff side of `scripts/vanilla-parity-run.sh`. Opt-in: set `TUFF_PARITY_OUT`
//! to the script's output directory and `TUFF_PARITY_SCRIPT` to the same
//! console script. Applies `tuff-stage-config.txt`, replays the script with a
//! bot attached and writes `tuff-events.txt` for `parity-diff`.

use std::path::PathBuf;
use std::time::Duration;

use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig, parity};

fn script_steps(text: &str) -> Vec<Step> {
    text.lines()
        .filter_map(|line| {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                None
            } else if let Some(secs) = line.strip_prefix("sleep ") {
                secs.trim().parse().ok().map(Step::Sleep)
            } else {
                Some(Step::Command(line.to_owned()))
            }
        })
        .collect()
}

#[derive(Debug, PartialEq)]
enum Step {
    Sleep(f64),
    Command(String),
}

fn stage_overrides(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| line.trim().split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn script_text_parses_into_steps_and_overrides() {
    let steps = script_steps("# c\nforceload add 0 0 1 1 # x\nsleep 2.5\n\nsay hi");
    assert_eq!(
        steps,
        [
            Step::Command("forceload add 0 0 1 1".into()),
            Step::Sleep(2.5),
            Step::Command("say hi".into()),
        ]
    );
    assert_eq!(
        stage_overrides("tuff.worldgen.debug.disableSurface=true\n"),
        [("tuff.worldgen.debug.disableSurface".into(), "true".into())]
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replay_parity_script_and_write_tuff_events() {
    let (Some(out), Some(script)) = (
        std::env::var_os("TUFF_PARITY_OUT").map(PathBuf::from),
        std::env::var_os("TUFF_PARITY_SCRIPT").map(PathBuf::from),
    ) else {
        return;
    };
    let steps = script_steps(&std::fs::read_to_string(script).expect("script"));
    let overrides = std::fs::read_to_string(out.join("tuff-stage-config.txt"))
        .map(|t| stage_overrides(&t))
        .unwrap_or_default();
    let seed = std::env::var("SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let instance = TempDir::new();
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(seed)
        .config_overrides(overrides)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            keep: parity::CAPTURE_IDS.to_vec(),
            ..BotConfig::new("Parity")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    for step in steps {
        match step {
            Step::Sleep(secs) => tokio::time::sleep(Duration::from_secs_f64(secs)).await,
            Step::Command(command) => {
                handle.run_command(&command).await.unwrap();
            }
        }
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let events = bot.with_state(|state| parity::normalize_kept(&state.kept_order));
    std::fs::write(out.join("tuff-events.txt"), events.join("\n") + "\n").unwrap();

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
