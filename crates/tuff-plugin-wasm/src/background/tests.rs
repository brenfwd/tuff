use std::time::Duration;

use tokio::sync::mpsc;

use super::*;
use crate::net::Delivery;
use crate::wit::canon::Value;
use crate::wit::testing::{self, Data, OUT, export};
use crate::wit::value::ToValue;

/// A plugin whose task is chosen by the first byte of its name: `e` echoes
/// the input, `b` returns 16 bytes, `f` fails with "nope", `t` calls a host
/// import (which traps) and `l` never ends.
fn module() -> Vec<u8> {
    let mut data = Data::new();
    let big: Result<Vec<u8>, String> = Ok(vec![7; 16]);
    let big = data.export_result("run-task", &big.to_value());
    let nope: Result<Vec<u8>, String> = Err("nope".into());
    let nope = data.export_result("run-task", &nope.to_value());
    let log = data.call("log", &[Value::Variant(0, None), Value::String("x".into())]);
    let body = format!(
        "(func {run}
           (local $kind i32)
           (local.set $kind (i32.load8_u (local.get 0)))
           (if (i32.eq (local.get $kind) (i32.const 116)) (then {log}))
           (if (i32.eq (local.get $kind) (i32.const 108)) (then (loop $spin (br $spin))))
           (if (i32.eq (local.get $kind) (i32.const 98)) (then (return (i32.const {big}))))
           (if (i32.eq (local.get $kind) (i32.const 102)) (then (return (i32.const {nope}))))
           (i32.store (i32.const {OUT}) (i32.const 0))
           (i32.store (i32.const {ptr}) (local.get 2))
           (i32.store (i32.const {len}) (local.get 3))
           (i32.const {OUT}))
         {data}",
        run = export("run-task"),
        ptr = OUT + 4,
        len = OUT + 8,
        data = data.wat(),
    );
    wat::parse_str(testing::module(&body)).unwrap()
}

struct Rig {
    background: Background,
    events: mpsc::UnboundedReceiver<Delivery>,
}

fn rig(limits: Limits) -> Rig {
    rig_with(module(), limits)
}

fn rig_with(module: Vec<u8>, limits: Limits) -> Rig {
    let (tx, events) = mpsc::unbounded_channel();
    let events_in = Events::new("p", 1, 16, tx);
    Rig {
        background: Background::new(module, limits, events_in),
        events,
    }
}

impl Rig {
    async fn next(&mut self) -> BackgroundDone {
        let delivery = tokio::time::timeout(Duration::from_secs(20), self.events.recv())
            .await
            .expect("a background result")
            .expect("the queue is open");
        match delivery.event {
            IoEvent::Background(done) => done,
            other => panic!("unexpected event {other:?}"),
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_task_runs_off_thread_and_delivers_its_output() {
    let mut rig = rig(Limits::default());
    let handle = rig.background.run("echo", b"hello").unwrap();
    assert!(handle >= HANDLE_BASE);
    let done = rig.next().await;
    assert_eq!(done.handle, handle);
    assert_eq!(done.outcome, Ok(b"hello".to_vec()));
    assert_eq!(rig.background.in_flight(), 0);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn handles_are_never_reused() {
    let mut rig = rig(Limits::default());
    let a = rig.background.run("echo", b"a").unwrap();
    rig.next().await;
    let b = rig.background.run("echo", b"b").unwrap();
    rig.next().await;
    assert!(b > a);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_task_error_is_reported_and_the_instance_is_kept() {
    let mut rig = rig(Limits::default());
    rig.background.run("fail", b"").unwrap();
    assert_eq!(
        rig.next().await.outcome,
        Err(BackgroundError::Failed("nope".into()))
    );
    rig.background.run("echo", b"again").unwrap();
    assert_eq!(rig.next().await.outcome, Ok(b"again".to_vec()));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_task_calling_the_host_is_faulted() {
    let mut rig = rig(Limits::default());
    rig.background.run("trap", b"").unwrap();
    assert!(matches!(
        rig.next().await.outcome,
        Err(BackgroundError::Faulted(_))
    ));
    rig.background.run("echo", b"after").unwrap();
    assert_eq!(rig.next().await.outcome, Ok(b"after".to_vec()));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_task_that_runs_out_of_fuel_is_faulted() {
    let mut rig = rig(Limits {
        fuel: 100_000,
        ..Limits::default()
    });
    rig.background.run("loop", b"").unwrap();
    assert!(matches!(
        rig.next().await.outcome,
        Err(BackgroundError::Faulted(_))
    ));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn output_over_the_message_limit_is_refused() {
    let mut rig = rig(Limits {
        message_bytes: 8,
        ..Limits::default()
    });
    rig.background.run("big", b"").unwrap();
    assert_eq!(rig.next().await.outcome, Err(BackgroundError::TooLarge));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn bad_names_and_big_input_are_denied_up_front() {
    let rig = rig(Limits {
        message_bytes: 8,
        ..Limits::default()
    });
    assert!(matches!(
        rig.background.run("", b""),
        Err(Denied::Malformed(_))
    ));
    assert!(matches!(
        rig.background.run(&"x".repeat(MAX_TASK_NAME + 1), b""),
        Err(Denied::Malformed(_))
    ));
    assert!(matches!(
        rig.background.run("echo", &[0; 9]),
        Err(Denied::Quota(_))
    ));
    assert_eq!(rig.background.in_flight(), 0);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tasks_in_flight_are_capped_and_cancelling_frees_a_place() {
    let mut rig = rig(Limits::default());
    let handles: Vec<Handle> = (0..MAX_IN_FLIGHT)
        .map(|_| rig.background.run("echo", b"x").unwrap())
        .collect();
    assert!(matches!(
        rig.background.run("echo", b"x"),
        Err(Denied::Quota(_))
    ));
    rig.background.cancel(handles[0]);
    let mut done = Vec::new();
    for _ in 1..MAX_IN_FLIGHT {
        done.push(rig.next().await.handle);
    }
    done.sort_unstable();
    assert_eq!(done, handles[1..]);
    assert_eq!(rig.background.in_flight(), 0);
    assert!(rig.background.run("echo", b"x").is_ok());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_cancelled_task_sends_no_event() {
    let mut rig = rig(Limits::default());
    let cancelled = rig.background.run("echo", b"gone").unwrap();
    rig.background.cancel(cancelled);
    let kept = rig.background.run("echo", b"kept").unwrap();
    assert_eq!(rig.next().await.handle, kept);
    let more = tokio::time::timeout(Duration::from_millis(200), rig.events.recv()).await;
    assert!(more.is_err());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_module_without_the_export_faults_its_tasks() {
    let body = format!("(func {} {})", export("register"), testing::ok_unit());
    let bare = wat::parse_str(testing::module(&body)).unwrap();
    let mut rig = rig_with(bare, Limits::default());
    rig.background.run("echo", b"").unwrap();
    assert!(matches!(
        rig.next().await.outcome,
        Err(BackgroundError::Faulted(_))
    ));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn dropping_the_plugin_drops_its_queued_tasks() {
    let Rig {
        background,
        mut events,
    } = rig(Limits::default());
    background.run("echo", b"x").unwrap();
    drop(background);
    let late = tokio::time::timeout(Duration::from_millis(300), events.recv()).await;
    assert!(!matches!(late, Ok(Some(_))));
}
