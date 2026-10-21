//! `tests/guests/rust-background`: background tasks through the Rust SDK's
//! `background::run(..).await`, driven the way the host does: the command
//! starts the task, the result arrives later as an `io-event` and resumes
//! the plugin's future. Skipped when the guest is not built, unless
//! `TUFF_EXAMPLE_PLUGINS=1` requires it.

use std::path::Path;
use std::time::Duration;

use tokio::sync::mpsc;
use tuff_plugin_wasm::net::{Delivery, Events};
use tuff_plugin_wasm::sandbox::{Limits, Player, Sandbox, engine};

fn built() -> Option<Vec<u8>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "tests/guests/rust-background/target/wasm32-unknown-unknown/release/background_guest.wasm",
    );
    match std::fs::read(&path) {
        Ok(wasm) => Some(wasm),
        Err(_) if std::env::var("TUFF_EXAMPLE_PLUGINS").as_deref() == Ok("1") => {
            panic!("{} is not built", path.display())
        }
        Err(_) => {
            eprintln!("skipping: {} is not built", path.display());
            None
        }
    }
}

fn plugin(wasm: &[u8]) -> (Sandbox, mpsc::UnboundedReceiver<Delivery>) {
    let mut plugin = Sandbox::new(&engine(), wasm, "bgguest", Limits::default()).unwrap();
    plugin.register().unwrap();
    plugin.enable().unwrap();
    let (queue, events) = mpsc::unbounded_channel();
    plugin.attach_background(Events::new("bgguest", 1, 8, queue));
    (plugin, events)
}

async fn answer(command: &str) -> Option<Vec<String>> {
    let wasm = built()?;
    let (mut plugin, mut events) = plugin(&wasm);
    let (result, effects) = plugin.command(Player::new(1, "Alex"), command).unwrap();
    assert_eq!(result, Ok(1));
    assert!(effects.replies.is_empty() && effects.tells.is_empty());
    let delivery = tokio::time::timeout(Duration::from_secs(20), events.recv())
        .await
        .unwrap()
        .unwrap();
    let effects = plugin.io(&delivery.event).unwrap();
    Some(effects.tells.into_iter().map(|(_, text)| text).collect())
}

#[tuff_core::watchdog]
#[tokio::test]
async fn awaiting_a_background_call_gives_its_output() {
    let Some(told) = answer("bg sum abc").await else {
        return;
    };
    assert_eq!(told, ["sum 294"]);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_failing_task_resolves_to_its_error() {
    let Some(told) = answer("bg bad").await else {
        return;
    };
    assert_eq!(told, ["failed nope"]);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_task_that_calls_the_host_is_faulted() {
    let Some(told) = answer("bg trap").await else {
        return;
    };
    assert_eq!(told, ["faulted"]);
}

#[tuff_core::watchdog]
#[test]
fn without_a_background_host_the_call_is_refused_at_once() {
    let Some(wasm) = built() else { return };
    let mut plugin = Sandbox::new(&engine(), &wasm, "bgguest", Limits::default()).unwrap();
    plugin.register().unwrap();
    plugin.enable().unwrap();
    let (_, effects) = plugin
        .command(Player::new(1, "Alex"), "bg sum abc")
        .unwrap();
    assert_eq!(effects.replies.len(), 1);
    assert!(
        effects.replies[0].starts_with("error refused"),
        "{effects:?}"
    );
}
