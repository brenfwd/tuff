//! `/tuff config` and `/tuff execute` as an operator's session.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::world::session::PlayerSession;
use tuff_item::text::TextComponent;
use tuff_storage::admin::PermissionLevel;

fn operator(level: PermissionLevel) -> PlayerSession {
    let (coordinator, mut requests) = mpsc::channel(16);
    tokio::spawn(async move { while requests.recv().await.is_some() {} });
    let mut shared = bare_shared();
    shared.coordinator = coordinator;
    shared.scoreboard =
        crate::world::scoreboard::start(shared.backend.clone(), shared.clock.clone()).0;
    let (outbound, _outbound) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(1000);
    let (_packets, packets) = mpsc::channel(8);
    PlayerSession::new(
        Arc::new(shared),
        1000,
        GameProfile {
            id: 1000,
            name: "Admin".to_owned(),
            properties: Vec::new(),
        },
        ClientInformation::default(),
        outbound,
        handle,
        events,
        packets,
        level,
    )
}

async fn result_of(session: &mut PlayerSession, input: &str) -> Result<i32, TextComponent> {
    let source = session.command_source();
    let registry = session.shared().commands.clone();
    let parsed = registry
        .roots()
        .parse(input, source.permission, source.parse_context());
    if let Some(error) = parsed.error(input) {
        return Err((*error.message).clone());
    }
    let executor = registry.executor(parsed.nodes[0].node.name).unwrap();
    let mut ctx = commands::Context {
        actor: commands::Actor::Player(session),
        source,
        nodes: &parsed.nodes,
        rest: &[],
        input,
        needs_result: true,
        frame: None,
    };
    executor
        .execute(&mut ctx)
        .await
        .map_err(|error| *error.message)
}

async fn score(session: &PlayerSession, holder: &str, objective: &str) -> Option<i32> {
    let (holder, objective) = (holder.to_owned(), objective.to_owned());
    session
        .shared()
        .scoreboard
        .read(move |board| board.score(&holder, &objective).map(|s| s.value))
        .await
        .flatten()
}

struct Script(std::path::PathBuf);

impl Script {
    fn new(name: &str, body: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::path::PathBuf::from("target/tuff-execute-tests")
            .join(format!("{}_{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        Script(path)
    }

    fn arg(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for Script {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn config_shows_sets_and_rejects() {
    let mut session = operator(PermissionLevel::Owners);
    assert_eq!(
        result_of(&mut session, "tuff config tuff.http.port").await,
        Ok(1)
    );
    assert_eq!(
        result_of(&mut session, "tuff config tuff.http.port 8123").await,
        Ok(1)
    );
    assert_eq!(
        session.shared().config.get_port("tuff.http.port"),
        Some(8123)
    );
    assert!(
        result_of(&mut session, "tuff config tuff.http.port 99999")
            .await
            .is_err()
    );
    assert!(
        result_of(&mut session, "tuff config tuff.nope")
            .await
            .is_err()
    );
    assert_eq!(
        result_of(&mut session, "tuff config tuff.http.host ::").await,
        Ok(1)
    );
    assert_eq!(
        session.shared().config.get_ip("tuff.http.host"),
        Some("::".parse().unwrap())
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tuff_commands_are_owner_only() {
    let mut session = operator(PermissionLevel::Admins);
    assert!(
        result_of(&mut session, "tuff config tuff.http.port")
            .await
            .is_err()
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_runs_lines_skipping_comments_and_counting_failures() {
    let mut session = operator(PermissionLevel::Owners);
    let script = Script::new(
        "mixed.txt",
        "# set up\n\n/scoreboard objectives add t dummy\nscoreboard players set #a t 5\nnot_a_command\nscoreboard players set #b t 7\n",
    );
    let result = result_of(&mut session, &format!("tuff execute {}", script.arg())).await;
    assert_eq!(result, Ok(3));
    assert_eq!(score(&session, "#a", "t").await, Some(5));
    assert_eq!(score(&session, "#b", "t").await, Some(7));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_can_stop_on_the_first_error() {
    let mut session = operator(PermissionLevel::Owners);
    let script = Script::new(
        "stops.txt",
        "scoreboard objectives add s dummy\nnot_a_command\nscoreboard players set #late s 1\n",
    );
    let result = result_of(
        &mut session,
        &format!("tuff execute {} --stop-on-error", script.arg()),
    )
    .await;
    let message = result.unwrap_err().plain();
    assert!(message.contains("line 2"), "{message}");
    assert_eq!(score(&session, "#late", "s").await, None);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_as_console_uses_console_permission() {
    let mut session = operator(PermissionLevel::Owners);
    let script = Script::new(
        "console.txt",
        "scoreboard objectives add c dummy\nscoreboard players set #x c 3\n",
    );
    let result = result_of(
        &mut session,
        &format!("tuff execute {} as console", script.arg()),
    )
    .await;
    assert_eq!(result, Ok(2));
    assert_eq!(score(&session, "#x", "c").await, Some(3));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_refuses_paths_outside_the_working_directory() {
    let mut session = operator(PermissionLevel::Owners);
    for path in ["../Cargo.toml", "/etc/hosts", "target/../../x"] {
        let result = result_of(&mut session, &format!("tuff execute {path}")).await;
        let message = result.unwrap_err().plain();
        assert!(
            message.contains("outside the working directory"),
            "{message}"
        );
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_caps_size_and_lines_and_refuses_nesting() {
    let mut session = operator(PermissionLevel::Owners);
    let many = Script::new("many.txt", &"# x\n".repeat(10_001));
    let message = result_of(&mut session, &format!("tuff execute {}", many.arg()))
        .await
        .unwrap_err()
        .plain();
    assert!(message.contains("lines"), "{message}");
    let big = Script::new("big.txt", &"#".repeat(2 << 20));
    let message = result_of(&mut session, &format!("tuff execute {}", big.arg()))
        .await
        .unwrap_err()
        .plain();
    assert!(message.contains("larger"), "{message}");

    let nested = Script::new("nested.txt", "scoreboard objectives add n dummy\n");
    let outer = Script::new("outer.txt", &format!("tuff execute {}\n", nested.arg()));
    assert_eq!(
        result_of(&mut session, &format!("tuff execute {}", outer.arg())).await,
        Ok(0)
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn config_keys_complete_from_the_live_store() {
    let session = operator(PermissionLevel::Owners);
    let store = session.shared().config.clone();
    let executor = session.shared().commands.executor("tuff").unwrap();
    let permissions = session.shared().permissions.view();
    let input = commands::SuggestInput {
        dimension: "minecraft:overworld",
        player_names: &[],
        config: &store,
        tracer: &session.shared().tracer,
        datapacks: &session.shared().datapacks,
        permissions: &permissions,
        services: &session.shared().services,
    };
    let registry = session.shared().commands.clone();
    let suggest = |text: &str| {
        let custom = |_p: &str, path: &[crate::command::ParsedNode], partial: &str| {
            executor.suggest(path, partial, &input)
        };
        registry.roots().suggest(
            text,
            PermissionLevel::Owners,
            &crate::command::SuggestContext::default(),
            &custom,
        )
    };
    let before = suggest("/tuff config tuff.ht");
    assert!(before.matches.contains(&"tuff.http.port".to_owned()));
    assert!(!before.matches.contains(&"late.key".to_owned()));
    assert_eq!(before.start, "/tuff config ".len());
    assert_eq!(before.length, "tuff.ht".len());

    store
        .register(crate::config::Key::new(
            "late.key",
            crate::config::Value::Bool(false),
            "added at run time",
        ))
        .unwrap();
    assert!(
        suggest("/tuff config late")
            .matches
            .contains(&"late.key".to_owned())
    );
    let values = suggest("/tuff config late.key ");
    assert_eq!(values.matches, vec!["false".to_owned(), "true".to_owned()]);
    assert_eq!(
        executor
            .describe("/tuff config late", "late.key", &input)
            .unwrap()
            .plain(),
        "bool: added at run time"
    );
}
