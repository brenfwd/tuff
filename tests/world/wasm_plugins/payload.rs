//! Custom payload channels: inbound client payloads reach only the
//! plugins that registered the channel, and sends need a registered one.

use super::*;
use bytes::Bytes;
use tuff_plugin_wasm::payload::Payloads;

const CHANNEL: &str = "tuff:test";

fn echo(id: &str) -> Vec<u8> {
    let mut data = Data::new();
    let more = data.call("register-channel", &[s(CHANNEL)]);
    let register = register(&mut data, &["ping"], &more);
    let uuid = WitValue::Record(vec![WitValue::U64(0), WitValue::U64(77)]);
    let ok = data.call(
        "send-payload",
        &[
            uuid.clone(),
            s(CHANNEL),
            WitValue::Bytes(vec![1, 2, 3].into()),
        ],
    );
    let bad = data.call(
        "send-payload",
        &[uuid, s("tuff:other"), WitValue::Bytes(vec![1].into())],
    );
    let body = format!(
        r#"{register}
        (func {run} (param i32 i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)
          {ok}
          (call $clear)
          (call $put_int (i32.load8_u (i32.const {RET})))
          {bad}
          (call $put_char (i32.const 32))
          (call $put_int (i32.load8_u (i32.const {RET})))
          {SAY}
          {done})
        (func {event} (param $ev i32)
          (call $clear)
          (call $put (i32.load offset=32 (local.get $ev)) (i32.load offset=36 (local.get $ev)))
          (call $put_char (i32.const 32))
          (call $put_int (i32.load offset=44 (local.get $ev)))
          {SHOUT})
        {data}"#,
        run = export("run-command").replace(
            " (param i32 i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)",
            ""
        ),
        done = ok_code("(i32.const 0)"),
        event = export("on-event").replace(" (param i32)", ""),
        data = data.wat(),
    );
    module(&plain(id), &body)
}

fn payload(channel: &str, data: &[u8]) -> Serverbound {
    Serverbound::CustomPayload {
        channel: channel.into(),
        data: Bytes::copy_from_slice(data),
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registered_channels_deliver_payloads_and_unregistered_sends_fail() {
    let dir = TestDir::new();
    install(dir.path(), "echo.wasm", &echo("echo"));
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);

    assert_eq!(
        Payloads
            .intercept(&mut client.session, &payload(CHANNEL, &[9, 8, 7, 6]))
            .await,
        Verdict::Cancel
    );
    assert_eq!(
        Payloads
            .intercept(&mut client.session, &payload("tuff:nobody", &[1]))
            .await,
        Verdict::Allow
    );
    assert_eq!(announced(&world).await, [format!("{CHANNEL} 4")]);

    assert_eq!(console(&world, "ping").await, ["0 1"]);
    world.shutdown().await;
}
