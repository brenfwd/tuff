use std::time::Duration;

use tuff_client_harness::launch::{
    LaunchSpec, RunDir, build_command, options_txt, servers_dat, uuid_no_dashes,
};
use tuff_client_harness::lock::{ClientLock, LockOwner, wait_pid_gone};
use tuff_client_harness::log::{ClientEvent, LogTail, parse_line};
use tuff_client_harness::meta::{Platform, VersionMeta};
use tuff_client_harness::supervisor::{ClientHandle, SuperviseConfig};
use tuff_client_harness::{ClientFiles, launch::LaunchCommand};
use tuff_nbt::{NamedTag, NbtLimits};

const FIXTURE: &str = include_str!("fixtures/version.json");

static FD_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn scratch(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("tuff-client-harness-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tuff_core::watchdog]
#[test]
fn lock_excludes_second_handle_and_records_owner() {
    let _guard = FD_GUARD.blocking_lock();
    let path = scratch("lock").join("c.lock");
    let a = ClientLock::try_acquire(&path, LockOwner::current("first"))
        .unwrap()
        .unwrap();
    assert!(
        ClientLock::try_acquire(&path, LockOwner::current("second"))
            .unwrap()
            .is_none()
    );
    let err = ClientLock::acquire(
        &path,
        &LockOwner::current("second"),
        Duration::from_millis(50),
        Duration::from_millis(10),
        |holder| assert_eq!(holder.unwrap().test, "first"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("first"), "{err}");
    drop(a);
    let b = ClientLock::try_acquire(&path, LockOwner::current("third"))
        .unwrap()
        .unwrap();
    assert_eq!(b.previous_owner().unwrap().test, "first");
}

#[tuff_core::watchdog]
#[test]
fn four_slots_then_queue_then_release() {
    let _guard = FD_GUARD.blocking_lock();
    let base = scratch("slots").join("c.lock");
    let n = tuff_client_harness::lock::CLIENT_SLOTS;
    assert_eq!(n, 4);
    let mut held: Vec<_> = (0..n)
        .map(|i| {
            ClientLock::try_acquire_slot(&base, n, &LockOwner::current(&format!("t{i}")))
                .unwrap()
                .unwrap()
        })
        .collect();
    assert!(
        ClientLock::try_acquire_slot(&base, n, &LockOwner::current("fifth"))
            .unwrap()
            .is_none()
    );
    let err = ClientLock::acquire_slot(
        &base,
        n,
        &LockOwner::current("fifth"),
        Duration::from_millis(50),
        Duration::from_millis(10),
        |holders| assert_eq!(holders.len(), n),
    )
    .unwrap_err();
    assert!(err.to_string().contains("t0"), "{err}");
    held.remove(2);
    assert!(
        ClientLock::try_acquire_slot(&base, n, &LockOwner::current("fifth"))
            .unwrap()
            .is_some()
    );
}

#[tuff_core::watchdog]
#[test]
fn previous_client_pid_is_waited_for() {
    let _guard = FD_GUARD.blocking_lock();
    let path = scratch("prev").join("c.lock");
    let mut a = ClientLock::try_acquire(&path, LockOwner::current("a"))
        .unwrap()
        .unwrap();
    a.set_client_pid(Some(4242)).unwrap();
    drop(a);
    let b = ClientLock::try_acquire(&path, LockOwner::current("b"))
        .unwrap()
        .unwrap();
    assert_eq!(b.previous_owner().unwrap().client_pid, Some(4242));
    assert!(
        b.wait_previous_client(Duration::from_millis(50), |_| false)
            .is_ok()
    );
    let err = b
        .wait_previous_client(Duration::from_millis(50), |pid| pid == 4242)
        .unwrap_err();
    assert!(err.to_string().contains("4242"));
    assert!(wait_pid_gone(1, Duration::from_millis(10), |_| false).is_ok());
}

#[tuff_core::watchdog]
#[test]
fn owner_roundtrip() {
    let mut owner = LockOwner::current("t");
    owner.client_pid = Some(7);
    assert_eq!(LockOwner::parse(&owner.render()).unwrap(), owner);
    assert!(LockOwner::parse("garbage").is_none());
}

#[tuff_core::watchdog]
#[test]
fn log_lines_parse_into_events() {
    assert_eq!(
        parse_line("[12:00:00] [Render thread/INFO]: [CHAT] <Bot> hi"),
        Some(ClientEvent::Chat("<Bot> hi".into()))
    );
    assert_eq!(
        parse_line("[x] [Render thread/INFO]: Connecting to 127.0.0.1, 25570"),
        Some(ClientEvent::Connecting("127.0.0.1, 25570".into()))
    );
    assert!(matches!(
        parse_line("[x]: Client disconnected with reason: Timed out"),
        Some(ClientEvent::Disconnected(r)) if r == "Timed out"
    ));
    assert!(matches!(
        parse_line("[x] [ERROR]: Error receiving packet 12"),
        Some(ClientEvent::ProtocolError(_))
    ));
    assert_eq!(parse_line("[x]: Setting user: Bot"), None);
    let mut tail = LogTail::default();
    for i in 0..80 {
        tail.push(format!("l{i}"));
    }
    assert_eq!(tail.len(), 50);
    assert!(tail.render().starts_with("l30"));
}

#[tuff_core::watchdog]
#[test]
fn game_dir_files() {
    let options = options_txt(4321);
    assert!(options.starts_with("version:4321\n"));
    assert!(options.contains("skipMultiplayerWarning:true\n"));
    assert!(options.contains("soundCategory_master:0.0\n"));
    let bytes = servers_dat("127.0.0.1:25570").unwrap();
    assert_eq!(bytes[0], 10);
    let (named, _) = NamedTag::from_bytes(&bytes, NbtLimits::TRUSTED).unwrap();
    let servers = named
        .tag
        .as_compound()
        .unwrap()
        .get_list("servers")
        .unwrap();
    let entry = servers[0].as_compound().unwrap();
    assert_eq!(entry.get_str("ip").unwrap(), "127.0.0.1:25570");
    assert_eq!(entry.get_byte("acceptTextures"), Some(1));
}

#[tuff_core::watchdog]
#[test]
fn command_line_is_built_from_version_json() {
    let meta = VersionMeta::parse(FIXTURE.as_bytes()).unwrap();
    let base = scratch("cmd");
    let run = RunDir::create(&base, "my test").unwrap();
    let platform = Platform {
        os: "osx",
        arch: "arm64",
    };
    let files = ClientFiles {
        version: "26.3".into(),
        meta,
        client_jar: base.join("client.jar"),
        classpath: vec![base.join("a.jar"), base.join("client.jar")],
        natives_dir: base.join("natives"),
        assets_dir: base.join("assets"),
        asset_index_id: "34".into(),
    };
    let java = base.join("java");
    let spec = LaunchSpec {
        java: &java,
        files: &files,
        run: &run,
        username: "Bot",
        address: "127.0.0.1:25570",
        platform: &platform,
        hook_args: &[],
        hook_dir: None,
    };
    let LaunchCommand { program, args } = build_command(&spec).unwrap();
    assert_eq!(program, java);
    assert_eq!(args[..2], ["-Xms512M", "-Xmx4G"]);
    assert!(args.iter().any(|a| a == "-XstartOnFirstThread"));
    assert!(args.iter().any(|a| a == "-XX:MaxDirectMemorySize=1G"));
    let hook = args
        .iter()
        .position(|a| a.ends_with("TuffClientHook.java"))
        .unwrap();
    assert!(
        args[..hook]
            .iter()
            .any(|a| a.starts_with("-Dtuff.harness.mainClass="))
    );
    let cp = args.iter().position(|a| a == "-cp").unwrap();
    assert!(cp < hook && args[cp + 1].contains("a.jar"));
    assert!(args[hook..].windows(2).any(|w| w == ["--username", "Bot"]));
    assert!(
        run.root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("my-test-")
    );
    assert_eq!(uuid_no_dashes(1).len(), 32);
}

#[cfg(unix)]
#[tuff_core::watchdog]
#[tokio::test]
async fn supervisor_streams_events_and_releases_lock_after_reap() {
    let _guard = FD_GUARD.lock().await;
    let dir = scratch("sup");
    let lock_path = dir.join("c.lock");
    let lock = ClientLock::try_acquire(&lock_path, LockOwner::current("sup"))
        .unwrap()
        .unwrap();
    let handle = ClientHandle::spawn(SuperviseConfig {
        command: LaunchCommand {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "echo '[CHAT] hello'; echo 'Error receiving packet 3' >&2; cat >/dev/null".into(),
            ],
        },
        control: None,
        log_path: dir.join("client.log"),
        grace: Duration::from_secs(5),
        lock,
    })
    .unwrap();
    let mut rx = handle.subscribe();
    let chat = handle
        .wait_event(&mut rx, Duration::from_secs(10), |e| {
            matches!(e, ClientEvent::Chat(_))
        })
        .await
        .unwrap();
    assert_eq!(chat, ClientEvent::Chat("hello".into()));
    handle
        .wait_event(&mut rx, Duration::from_secs(10), |e| {
            matches!(e, ClientEvent::ProtocolError(_))
        })
        .await
        .unwrap();
    assert!(
        ClientLock::try_acquire(&lock_path, LockOwner::current("x"))
            .unwrap()
            .is_none()
    );
    let status = handle.shutdown().await.unwrap();
    assert!(status.success());
    assert!(
        ClientLock::try_acquire(&lock_path, LockOwner::current("x"))
            .unwrap()
            .is_some()
    );
    assert!(
        std::fs::read_to_string(dir.join("client.log"))
            .unwrap()
            .contains("[CHAT] hello")
    );
}

#[cfg(unix)]
#[tuff_core::watchdog]
#[tokio::test]
async fn supervisor_kills_after_grace_and_reports_tail_on_timeout() {
    let _guard = FD_GUARD.lock().await;
    let dir = scratch("kill");
    let lock = ClientLock::try_acquire(&dir.join("c.lock"), LockOwner::current("k"))
        .unwrap()
        .unwrap();
    let handle = ClientHandle::spawn(SuperviseConfig {
        command: LaunchCommand {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "trap '' HUP; echo started; while true; do sleep 1; done".into(),
            ],
        },
        control: None,
        log_path: dir.join("client.log"),
        grace: Duration::from_millis(200),
        lock,
    })
    .unwrap();
    let mut rx = handle.subscribe();
    let err = handle
        .wait_event(&mut rx, Duration::from_millis(500), |_| false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("started"), "{err}");
    let status = handle.shutdown().await.unwrap();
    assert!(!status.success());
    assert!(handle.has_exited().await);
}

#[tuff_core::watchdog]
#[test]
fn hook_cache_key_changes_with_source_classpath_and_java() {
    let cp = vec![std::path::PathBuf::from("a.jar")];
    let j = std::path::Path::new("/j/bin/java");
    let base = tuff_client_harness::launch::hook_cache_key(b"src", &cp, j);
    assert_eq!(
        base,
        tuff_client_harness::launch::hook_cache_key(b"src", &cp, j)
    );
    assert_ne!(
        base,
        tuff_client_harness::launch::hook_cache_key(b"src2", &cp, j)
    );
    assert_ne!(
        base,
        tuff_client_harness::launch::hook_cache_key(
            b"src",
            &[std::path::PathBuf::from("b.jar")],
            j
        )
    );
    assert_ne!(
        base,
        tuff_client_harness::launch::hook_cache_key(
            b"src",
            &cp,
            std::path::Path::new("/k/bin/java")
        )
    );
}

fn hook_cache_scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tuff-hook-cache-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tuff_core::watchdog]
#[test]
fn hook_cache_hit_returns_existing_dir_without_javac() {
    use tuff_client_harness::launch::{HOOK_SOURCE, build_hook, hook_cache_key};
    let cache = hook_cache_scratch("hit");
    let java = std::path::Path::new("/nonexistent/bin/java");
    let cp = vec![std::path::PathBuf::from("a.jar")];
    let source = std::fs::read(HOOK_SOURCE).unwrap();
    let dir = cache.join(hook_cache_key(&source, &cp, java));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("TuffClientHook.class"), b"cached").unwrap();
    assert_eq!(build_hook(java, &cp, &cache).unwrap(), dir);
    assert_eq!(
        std::fs::read(dir.join("TuffClientHook.class")).unwrap(),
        b"cached"
    );
    let _ = std::fs::remove_dir_all(&cache);
}

#[tuff_core::watchdog]
#[test]
fn hook_cache_miss_does_not_reuse_other_key_and_failed_javac_leaves_no_tmp() {
    use tuff_client_harness::launch::{HOOK_SOURCE, build_hook, hook_cache_key};
    let cache = hook_cache_scratch("miss");
    let java = std::path::Path::new("/nonexistent/bin/java");
    let cp = vec![std::path::PathBuf::from("a.jar")];
    let source = std::fs::read(HOOK_SOURCE).unwrap();
    let dir = cache.join(hook_cache_key(&source, &cp, java));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("TuffClientHook.class"), b"cached").unwrap();
    let other = vec![std::path::PathBuf::from("/nonexistent/b.jar")];
    assert!(build_hook(java, &other, &cache).is_err());
    let names: Vec<String> = std::fs::read_dir(&cache)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec![dir.file_name().unwrap().to_string_lossy().into_owned()]
    );
    let _ = std::fs::remove_dir_all(&cache);
}
