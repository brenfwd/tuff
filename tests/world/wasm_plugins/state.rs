//! Game mode, hunger and dimension events and the state filters, end to
//! end through a WAT plugin.

use super::*;
use crate::protocol::play::GameMode;
use crate::world::messages::Teleport;
use crate::world::session::hooks::SessionHooks;
use crate::world::session::hooks::SessionTick;
use tuff_plugin_wasm::state::States;
use tuff_plugin_wasm::wit::bindings::types::{BlockPos, RegionWatch, StateFilter, StateKind};

/// Refuses switching to creative and food losses, and shouts the first
/// string of each game mode and world change event.
fn lock() -> Vec<u8> {
    let mut data = Data::new();
    let filter = |kind, game_mode: Option<&str>| {
        StateFilter {
            kind,
            player: None,
            dimension: None,
            game_mode: game_mode.map(Into::into),
        }
        .to_value()
        .into_owned()
    };
    let more = data.call(
        "intercept-state",
        &[filter(StateKind::GameMode, Some("creative"))],
    ) + &data.call("intercept-state", &[filter(StateKind::HungerLoss, None)])
        + &data.call("subscribe", &[EventKinds::PLAYER_STATE.to_value()]);
    let register = register(&mut data, &[], &more);
    let body = format!(
        r#"{register}
        (func {event} (param $ev i32)
          (call $clear)
          (if (i32.and (i32.eq (i32.load8_u (local.get $ev)) (i32.const 31))
                       (i32.or (i32.eq (i32.load8_u offset=8 (local.get $ev)) (i32.const 2))
                               (i32.eq (i32.load8_u offset=8 (local.get $ev)) (i32.const 4))))
            (then
              (call $put (i32.load offset=40 (local.get $ev)) (i32.load offset=44 (local.get $ev)))
              {SHOUT}))
          )
        {data}"#,
        event = export("on-event").replace(" (param i32)", ""),
        data = data.wat(),
    );
    module(&plain("statelock"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn filters_refuse_game_modes_and_food_losses() {
    let dir = TestDir::new();
    install(dir.path(), "statelock.wasm", &lock());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);

    assert!(!client.session.set_game_mode(GameMode::Creative).await);
    assert_eq!(client.session.mode.game_mode, GameMode::Survival);
    assert!(client.session.set_game_mode(GameMode::Adventure).await);

    client.session.set_food_level(10);
    client.session.body.food.level = 4;
    client.session.food_changed_hooks(10);
    assert_eq!(client.session.food_level(), 10);
    client.session.body.food.level = 20;
    client.session.food_changed_hooks(10);
    assert_eq!(client.session.food_level(), 20);

    States.dimension_changed(&client.session, "minecraft:the_nether");

    assert_eq!(
        announced(&world).await,
        ["survival", "survival", "minecraft:the_nether"]
    );
    world.shutdown().await;
}

/// Watches a box and refuses `/tp`; shouts `name+`/`name-` for region
/// crossings and `tp-cancelled`/`tp-ok` for teleports.
fn zone() -> Vec<u8> {
    let mut data = Data::new();
    let region = RegionWatch {
        name: "spawn".into(),
        dimension: "minecraft:overworld".into(),
        min: BlockPos { x: 0, y: 60, z: 0 },
        max: BlockPos {
            x: 10,
            y: 80,
            z: 10,
        },
    };
    let filter = StateFilter {
        kind: StateKind::Teleport,
        player: None,
        dimension: None,
        game_mode: None,
    };
    let more = data.call("watch-region", &[region.to_value()])
        + &data.call("intercept-state", &[filter.to_value()])
        + &data.call("subscribe", &[EventKinds::PLAYER_STATE.to_value()]);
    let register = register(&mut data, &[], &more);
    let name =
        "(call $put (i32.load offset=40 (local.get $ev)) (i32.load offset=44 (local.get $ev)))";
    let plus = text(&mut data, "+");
    let minus = text(&mut data, "-");
    let cancelled = text(&mut data, "tp-cancelled");
    let allowed = text(&mut data, "tp-ok");
    let body = format!(
        r#"{register}
        (func {event} (param $ev i32)
          (call $clear)
          (if (i32.eq (i32.load8_u (local.get $ev)) (i32.const 31))
            (then
              (if (i32.eq (i32.load8_u offset=8 (local.get $ev)) (i32.const 6))
                (then
                  {name}
                  (if (i32.load8_u offset=48 (local.get $ev))
                    (then (call $put {plus}))
                    (else (call $put {minus})))
                  {SHOUT}))
              (if (i32.eq (i32.load8_u offset=8 (local.get $ev)) (i32.const 5))
                (then
                  (if (i32.load8_u offset=104 (local.get $ev))
                    (then (call $put {cancelled}))
                    (else (call $put {allowed})))
                  {SHOUT}))))
          )
        {data}"#,
        event = export("on-event").replace(" (param i32)", ""),
        data = data.wat(),
    );
    module(&plain("zone"), &body)
}

async fn move_to(client: &mut Client, pos: [f64; 3]) {
    client.session.pose.position = pos;
    client
        .session
        .run_session_tick_hooks(SessionTick::Late)
        .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watched_regions_report_entering_leaving_and_logging_out() {
    let dir = TestDir::new();
    install(dir.path(), "zone.wasm", &zone());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);

    move_to(&mut client, [50.0, 70.0, 50.0]).await;
    move_to(&mut client, [5.5, 70.0, 5.5]).await;
    move_to(&mut client, [6.5, 70.0, 6.5]).await;
    move_to(&mut client, [50.0, 70.0, 50.0]).await;
    move_to(&mut client, [5.5, 70.0, 5.5]).await;
    client.session.leaving_hooks().await;

    assert_eq!(
        announced(&world).await,
        ["spawn+", "spawn-", "spawn+", "spawn-"]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_teleport_leaves_the_player_and_reports_cancelled() {
    let dir = TestDir::new();
    install(dir.path(), "zone.wasm", &zone());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let start_at = client.session.position();

    let teleport = Teleport {
        dimension: client.session.dimension_id(),
        position: [200.5, 90.0, 200.5],
        relative: [false; 3],
        rotation: None,
        relative_rotation: [false; 2],
        look_at: None,
    };
    client.session.teleport_by_command(teleport).await;

    assert_eq!(client.session.position(), start_at);
    assert_eq!(announced(&world).await, ["tp-cancelled"]);
    world.shutdown().await;
}
