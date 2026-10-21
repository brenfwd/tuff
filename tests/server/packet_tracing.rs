use crate::common;

use std::path::{Path, PathBuf};

use common::TempDir;
use tuff::{Server, ServerHandle};
use tuff_bot::{Bot, BotConfig};

struct Line {
    ns: u128,
    state: String,
    direction: String,
    text: String,
}

fn parse(line: &str) -> Line {
    let mut parts = line.splitn(4, ' ');
    let ns = parts.next().unwrap().parse().unwrap();
    let state = parts.next().unwrap().to_owned();
    let direction = parts.next().unwrap().to_owned();
    let text = parts.next().unwrap().to_owned();
    assert!(
        ns > 1_000_000_000_000_000_000,
        "unix epoch nanoseconds: {ns}"
    );
    assert!(
        ["handshake", "status", "login", "configuration", "play"].contains(&state.as_str()),
        "{line}"
    );
    assert!(["sb", "cb"].contains(&direction.as_str()), "{line}");
    Line {
        ns,
        state,
        direction,
        text,
    }
}

fn read(path: &Path) -> Vec<Line> {
    let file = std::fs::File::open(path).unwrap();
    tuff_core::trace::dump::render_to_string(file, Default::default())
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(parse)
        .collect()
}

fn trace_files(dir: &TempDir) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
}

struct Fixture {
    handle: ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    addr: std::net::SocketAddr,
    traces: TempDir,
    _instance: TempDir,
}

async fn fixture() -> Fixture {
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
    Fixture {
        handle,
        task,
        addr,
        traces,
        _instance: instance,
    }
}

impl Fixture {
    async fn say(&self, command: &str) -> String {
        self.handle
            .run_command(command)
            .await
            .unwrap()
            .iter()
            .map(|t| t.plain())
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn join(&self, name: &str) -> Bot {
        let bot = Bot::connect(
            self.addr,
            BotConfig {
                view_distance: 2,
                ..BotConfig::new(name)
            },
        )
        .await
        .unwrap();
        bot.wait_for(|state| state.chunks > 0).await.unwrap();
        bot
    }

    async fn finish(self) {
        self.handle.shutdown().await.unwrap();
        self.task.await.unwrap().unwrap();
    }
}

fn first(lines: &[Line], state: &str, direction: &str) -> usize {
    lines
        .iter()
        .position(|l| l.state == state && l.direction == direction)
        .unwrap_or_else(|| panic!("no {state} {direction} line"))
}

#[tuff_core::watchdog]
#[tokio::test]
async fn trace_armed_before_login_captures_every_state_in_order() {
    let fx = fixture().await;
    let started = fx.say("tuff tracing Tester start").await;
    assert!(started.contains("Tracing Tester"), "{started}");
    let bot = fx.join("tester").await;
    drop(bot);
    let stopped = fx.say("tuff tracing TESTER stop").await;
    assert!(stopped.contains("Stopped tracing"), "{stopped}");

    let files = trace_files(&fx.traces);
    assert_eq!(files.len(), 1);
    let name = files[0].file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("tuff-trace-") && name.ends_with("-Tester.tuffrec"),
        "{name}"
    );

    let lines = read(&files[0]);
    assert_eq!(lines[0].state, "handshake");
    assert_eq!(lines[0].direction, "sb");
    assert!(lines[0].text.starts_with("Intention"), "{}", lines[0].text);
    let order: Vec<usize> = ["login", "configuration", "play"]
        .iter()
        .map(|s| first(&lines, s, "cb"))
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    for state in ["login", "configuration", "play"] {
        first(&lines, state, "sb");
    }
    let mut rank = 0;
    for line in &lines {
        let r = ["handshake", "login", "configuration", "play"]
            .iter()
            .position(|s| *s == line.state)
            .unwrap();
        assert!(r >= rank, "state went backwards at {}", line.text);
        rank = r;
    }
    for direction in ["sb", "cb"] {
        let times: Vec<u128> = lines
            .iter()
            .filter(|l| l.direction == direction)
            .map(|l| l.ns)
            .collect();
        assert!(times.windows(2).all(|w| w[0] <= w[1]));
    }
    assert!(
        lines
            .iter()
            .any(|l| l.direction == "sb" && l.text.starts_with("LoginStart"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.direction == "cb" && l.text.contains("minecraft:login_finished"))
    );
    fx.finish().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn trace_survives_a_kick_and_a_reconnect_until_stopped() {
    let fx = fixture().await;
    fx.say("tuff tracing Tester start").await;
    let bot = fx.join("Tester").await;
    fx.say("kick Tester").await;
    assert!(bot.wait_for(|_| false).await.is_err());
    drop(bot);
    let again = fx.join("Tester").await;
    drop(again);
    fx.say("tuff tracing Tester stop").await;

    let files = trace_files(&fx.traces);
    assert_eq!(files.len(), 1);
    let lines = read(&files[0]);
    let logins = lines
        .iter()
        .filter(|l| l.direction == "sb" && l.text.starts_with("LoginStart"))
        .count();
    assert_eq!(logins, 2);
    let kick = lines
        .iter()
        .position(|l| {
            l.state == "play" && l.direction == "cb" && l.text.contains("minecraft:disconnect")
        })
        .expect("kick packet traced");
    let second = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.text.starts_with("LoginStart"))
        .nth(1)
        .unwrap()
        .0;
    assert!(kick < second);
    fx.finish().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn stop_closes_the_file_and_start_opens_a_new_one() {
    let fx = fixture().await;
    let unarmed = fx.join("Other").await;
    drop(unarmed);
    assert!(trace_files(&fx.traces).is_empty());

    fx.say("tuff tracing Tester start").await;
    let duplicate = fx.say("tuff tracing tester start").await;
    assert!(duplicate.contains("Already tracing"), "{duplicate}");
    fx.say("tuff tracing Tester stop").await;
    let closed = read(&trace_files(&fx.traces)[0]).len();
    let missing = fx.say("tuff tracing Tester stop").await;
    assert!(missing.contains("Not tracing"), "{missing}");

    let bot = fx.join("Tester").await;
    drop(bot);
    assert_eq!(read(&trace_files(&fx.traces)[0]).len(), closed);

    fx.say("tuff tracing Tester start").await;
    let bot = fx.join("Tester").await;
    drop(bot);
    fx.say("tuff tracing Tester stop").await;
    let files = trace_files(&fx.traces);
    assert_eq!(files.len(), 2);
    let mut lens: Vec<usize> = files.iter().map(|f| read(f).len()).collect();
    lens.sort();
    assert_eq!(lens[0], closed);
    assert!(lens[1] > closed);
    fx.finish().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn clientbound_lines_carry_typed_debug_before_the_framed_form() {
    let fx = fixture().await;
    fx.say("tuff tracing Tester start").await;
    let bot = fx.join("Tester").await;
    drop(bot);
    fx.say("tuff tracing Tester stop").await;
    let lines = read(&trace_files(&fx.traces)[0]);
    let typed = |state: &str, head: &str, name: &str| {
        let line = lines
            .iter()
            .find(|l| l.state == state && l.direction == "cb" && l.text.starts_with(head))
            .unwrap_or_else(|| panic!("no {head} line"));
        let (typed, raw) = line.text.split_once(" | Packet { name: ").unwrap();
        assert!(typed.starts_with(head), "{typed}");
        assert!(raw.starts_with(&format!("\"{name}\"")), "{raw}");
    };
    typed("login", "LoginFinished { uuid", "minecraft:login_finished");
    typed("play", "Login { entity_id", "minecraft:login");
    typed(
        "configuration",
        "RegistryData { registry",
        "minecraft:registry_data",
    );
    for line in &lines {
        assert!(
            line.text.len() < 2000,
            "unbounded line: {}",
            line.text.len()
        );
    }
    let framed_only: Vec<_> = lines
        .iter()
        .filter(|l| l.direction == "cb" && l.state != "handshake" && l.text.starts_with("Packet {"))
        .map(|l| &l.text)
        .collect();
    assert!(
        framed_only.is_empty(),
        "untyped clientbound lines: {framed_only:?}"
    );
    fx.finish().await;
}
