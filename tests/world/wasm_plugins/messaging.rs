//! Plugin messaging: providers, requests, replies and notifies through
//! the host actor, with hand-written guests.

use super::*;

const IFACE: &str = "svc:echo";
const CALLBACK: &str = "cli:cb";
const ERRORS: &str = "e0e1e2e3e4e5e6e7e8";

/// Provides `svc:echo`: 1 echoes, 2 answers "late" from a timer, 3 traps,
/// 4 never answers, 5 requests `cli:cb` and answers "done" when that
/// replies, 6 notifies `cli:cb` back with 7 as the method.
fn provider() -> Vec<u8> {
    let mut data = Data::new();
    let iface = text(&mut data, IFACE);
    let callback = text(&mut data, CALLBACK);
    let late = text(&mut data, "late");
    let done = text(&mut data, "done");
    let ping = text(&mut data, "ping");
    let register = format!(
        "(func {} (call $provide {iface} (i32.const 3000) (i32.const 6) (i32.const {RET})) {})",
        export("register"),
        ok_unit()
    );
    let body = format!(
        "{register}
         (data (i32.const 3000) \"\\01\\00\\00\\00\\00\\00\\00\\00\\02\\00\\00\\00\\00\\00\\00\\00\\03\\00\\00\\00\\00\\00\\00\\00\\04\\00\\00\\00\\00\\00\\00\\00\\05\\00\\00\\00\\00\\00\\00\\00\\06\\00\\00\\00\\00\\00\\00\\00\")
         (func {} 
           (if (i64.eq (local.get 4) (i64.const 1))
             (then (call $respond (local.get 8) (i32.const 0) (local.get 5) (local.get 6) (i32.const {RET}))))
           (if (i64.eq (local.get 4) (i64.const 2))
             (then (i64.store (i32.const 7000) (local.get 8))
                   (call $schedule (i64.const 1) (i32.const 5) (i32.const {RET}))))
           (if (i64.eq (local.get 4) (i64.const 3)) (then unreachable))
           (if (i64.eq (local.get 4) (i64.const 5))
             (then (i64.store (i32.const 7000) (local.get 8))
                   (call $request (i32.const 0) (i32.const 0) (i32.const 0) {callback} (i64.const 1) {ping} (i32.const 0) (i32.const {RET}))))
           (if (i64.eq (local.get 4) (i64.const 6))
             (then (call $notify (i32.const 0) (i32.const 0) (i32.const 0) {callback} (i64.const 7) {ping} (i32.const {RET})))))
         (func {}
           (call $respond (i64.load (i32.const 7000)) (i32.const 0) {late} (i32.const {RET})))
         (func {}
           (if (i32.eq (local.get 0) (i32.const 10))
             (then (call $respond (i64.load (i32.const 7000)) (i32.const 0) {done} (i32.const {RET})))))
         {}",
        export("on-message"),
        export("on-timer"),
        export("on-io"),
        data.wat()
    );
    module(&plain("svc"), &body)
}

/// Commands: 0 echo, 1 later, 2 boom, 3 silent (5 tick timeout),
/// 4 nobody, 5 nested, 6 relay. Also provides `cli:cb` (1 answers "cb",
/// 7 notifies the provider's method 6 again) and prints every reply.
fn requester() -> Vec<u8> {
    let mut data = Data::new();
    let iface = text(&mut data, IFACE);
    let nobody = text(&mut data, "x:none");
    let callback = text(&mut data, CALLBACK);
    let hello = text(&mut data, "hello");
    let cb = text(&mut data, "cb");
    let up = text(&mut data, "up");
    let down = text(&mut data, "down");
    let pp = text(&mut data, "pp");
    let register = register(
        &mut data,
        &[
            "echo", "later", "boom", "silent", "nobody", "nested", "relay",
        ],
        &format!("(call $provide {callback} (i32.const 3104) (i32.const 2) (i32.const {RET}))"),
    );
    let ask = |handler: u32, method: u32, iface: &str, ticks: u32| {
        format!(
            "(if (i32.eq (local.get 0) (i32.const {handler}))
               (then (call $request (i32.const 0) (i32.const 0) (i32.const 0) {iface} (i64.const {method}) {hello} (i32.const {ticks}) (i32.const {RET}))
                     (if (i32.load8_u (i32.const {RET}))
                       (then (call $broadcast (i32.add (i32.const 8000) (i32.mul (i32.load8_u (i32.const {r4})) (i32.const 2))) (i32.const 2) (i32.const {RET}))))))",
            r4 = RET + 4
        )
    };
    let commands = [
        ask(0, 1, &iface, 0),
        ask(1, 2, &iface, 0),
        ask(2, 3, &iface, 0),
        ask(3, 4, &iface, 5),
        ask(4, 1, &nobody, 0),
        ask(5, 5, &iface, 0),
        format!(
            "(if (i32.eq (local.get 0) (i32.const 6))
               (then (call $notify (i32.const 0) (i32.const 0) (i32.const 0) {iface} (i64.const 6) {hello} (i32.const {RET}))))"
        ),
    ]
    .concat();
    let body = format!(
        "{register}
         (data (i32.const 3104) \"\\01\\00\\00\\00\\00\\00\\00\\00\\07\\00\\00\\00\\00\\00\\00\\00\")
         (data (i32.const 8000) \"{ERRORS}\")
         (func {} {commands} {})
         (func {} 
           (if (i64.eq (local.get 4) (i64.const 1))
             (then (call $respond (local.get 8) (i32.const 0) {cb} (i32.const {RET}))))
           (if (i64.eq (local.get 4) (i64.const 7))
             (then (call $broadcast {pp} (i32.const {RET}))
                   (call $notify (i32.const 0) (i32.const 0) (i32.const 0) {iface} (i64.const 6) {hello} (i32.const {RET})))))
         (func {} 
           (if (i32.eq (local.get 0) (i32.const 10))
             (then (if (i32.eqz (local.get 2))
                     (then (call $broadcast (local.get 3) (local.get 4) (i32.const {RET})))
                     (else (call $broadcast (i32.add (i32.const 8000) (i32.mul (local.get 3) (i32.const 2))) (i32.const 2) (i32.const {RET}))))))
           (if (i32.eq (local.get 0) (i32.const 11))
             (then (if (local.get 5)
                     (then (call $broadcast {up} (i32.const {RET})))
                     (else (call $broadcast {down} (i32.const {RET})))))))
         {}",
        export("run-command"),
        ok_code("(i32.const 0)"),
        export("on-message"),
        export("on-io"),
        data.wat()
    );
    module(&format!("{}\nsoft-depends = svc", plain("cli")), &body)
}

async fn run(world: &TestWorld, line: &str) {
    host(world)
        .command(Player::new(77, "Steve"), line.into())
        .await
        .unwrap();
    host(world).sync().await;
}

async fn setup() -> (TestDir, TestWorld) {
    let dir = TestDir::new();
    install(dir.path(), "svc.wasm", &provider());
    install(dir.path(), "cli.wasm", &requester());
    let world = start(dir.path()).await;
    if loaded(&world).await.len() != 2 {
        panic!(
            "{:?} {:?}",
            console(&world, "tuff plugin load svc").await,
            console(&world, "tuff plugin load cli").await
        );
    }
    (dir, world)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_reply_immediately_and_later_and_fail_fast() {
    let (_dir, world) = setup().await;
    run(&world, "echo").await;
    assert_eq!(announced(&world).await, ["hello"]);
    run(&world, "later").await;
    assert_eq!(announced(&world).await, ["hello"]);
    world.ticks(1).await;
    assert_eq!(announced(&world).await, ["hello", "late"]);
    run(&world, "nobody").await;
    assert_eq!(announced(&world).await.last().unwrap(), "e0");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trapping_silent_and_vanishing_providers_complete_their_requests() {
    let (_dir, world) = setup().await;
    run(&world, "silent").await;
    world.ticks(4).await;
    assert!(announced(&world).await.is_empty());
    world.ticks(1).await;
    assert_eq!(announced(&world).await, ["e5"]);
    run(&world, "later").await;
    console(&world, "tuff plugin unload svc").await;
    host(&world).sync().await;
    assert_eq!(announced(&world).await, ["e5", "e3", "down"]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_trapping_provider_faults_the_request_and_is_unloaded() {
    let (_dir, world) = setup().await;
    run(&world, "boom").await;
    assert_eq!(announced(&world).await.first().unwrap(), "e4");
    assert!(!loaded(&world).await.iter().any(|(id, _)| id == "svc"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nested_requests_finish_in_one_turn_and_loops_defer_to_the_next_tick() {
    let (_dir, world) = setup().await;
    run(&world, "nested").await;
    assert_eq!(announced(&world).await, ["done"]);
    let pings = |lines: &[String]| lines.iter().filter(|line| *line == "pp").count();
    run(&world, "relay").await;
    world.ticks(2).await;
    let total = pings(&announced(&world).await);
    assert!(
        total > abi::MAX_HOPS / 2 && total % (abi::MAX_HOPS / 2) == 0,
        "{total} pings: deferred hops must continue on later ticks, a hop budget at a time"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declared_soft_dependencies_hear_plugin_changes() {
    let (dir, world) = setup().await;
    console(&world, "tuff plugin unload svc").await;
    host(&world).sync().await;
    assert_eq!(announced(&world).await, ["down"]);
    console(&world, "tuff plugin load svc").await;
    host(&world).sync().await;
    assert_eq!(announced(&world).await, ["down", "up"]);
    drop(dir);
    world.shutdown().await;
}
