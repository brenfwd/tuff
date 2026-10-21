//! Settings and resource packs a runtime plugin declares: they live in
//! the server's configuration and pack list while the plugin is loaded, a
//! saved setting outlives the plugin, and changes reach it as events.

use super::*;
use tuff_plugin_wasm::wit::bindings::types::{ConfigDecl, ConfigValue, ResourcePack};

const SPEED: &str = "plugins.tuner.speed";

fn pack_zip() -> Vec<u8> {
    let v = tuff_vanilla_data::version::RESOURCE_PACK_VERSION;
    let meta = format!(
        r#"{{"pack":{{"description":"tuner","min_format":[{0},{1}],"max_format":[{0},{1}]}}}}"#,
        v.major, v.minor
    );
    tuff_zip::write_all(
        &[("pack.mcmeta", meta.as_bytes())],
        tuff_zip::DosTime::MIN,
        6,
        u64::MAX,
    )
    .unwrap()
}

/// Declares `speed` (default 1) and a pack, hears its setting change and
/// broadcasts `<key> <value>`; `/tune` sets the speed to 42 and `/speed`
/// answers it as its result code.
fn tuner() -> Vec<u8> {
    let mut data = Data::new();
    let declare = data.call(
        "declare-config",
        &[ConfigDecl {
            key: "speed".into(),
            default: ConfigValue::Integer(1),
            description: "how fast".into(),
            secret: false,
            text_component: false,
        }
        .to_value()],
    );
    let pack = data.call(
        "register-resource-pack",
        &[ResourcePack {
            name: "look".into(),
            data: pack_zip(),
            required: false,
            prompt: Some("Tuner's look".into()),
        }
        .to_value()],
    );
    let subscribe = data.call("subscribe", &[EventKinds::CONFIG_CHANGE.to_value()]);
    let ok = format!("(if (i32.load8_u (i32.const {RET})) (then unreachable))");
    let register = register(
        &mut data,
        &["tune", "speed"],
        &format!("{declare} {ok} {pack} {ok} {subscribe} {ok}"),
    );
    let set = data.call(
        "config-set",
        &[s("speed"), ConfigValue::Integer(42).to_value()],
    );
    let get = data.call("config-get", &[s("speed")]);
    let body = format!(
        r#"{register}
        (func {run} (param $handler i32) (param i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)
          (if (i32.eqz (local.get $handler))
            (then {set} (return {set_code})))
          {get}
          {got})
        (func {event} (param $ev i32)
          (call $clear)
          (call $put (i32.load offset=8 (local.get $ev)) (i32.load offset=12 (local.get $ev)))
          (call $put_char (i32.const 32))
          (call $put_int (i32.wrap_i64 (i64.load offset=24 (local.get $ev))))
          {SHOUT})
        {data}"#,
        run = export("run-command").replace(
            " (param i32 i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)",
            ""
        ),
        set_code = ok_code(&format!("(i32.load8_u (i32.const {RET}))")),
        got = ok_code(&format!(
            "(if (result i32) (i32.load8_u (i32.const {RET})) (then (i32.wrap_i64 (i64.load (i32.const {})))) (else (i32.const -1)))",
            RET + 16
        )),
        event = export("on-event").replace(" (param i32)", ""),
        data = data.wat(),
    );
    module(&plain("tuner"), &body)
}

async fn speed(world: &TestWorld) -> Result<i32, String> {
    host(world)
        .command(Player::new(77, "Steve"), "speed".into())
        .await
        .unwrap()
        .result
}

/// Waits until `line` has been announced `times` times.
async fn heard(world: &TestWorld, line: &str, times: usize) -> bool {
    for _ in 0..100 {
        if count(world, line).await >= times {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_settings_live_in_the_config_and_outlive_the_plugin() {
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    world.shared.config.load_saved([(SPEED, "9")]);
    install(dir.path(), "tuner.wasm", &tuner());
    assert_eq!(
        console(&world, "tuff plugin load tuner").await,
        ["Loaded tuner 1.0.0"]
    );
    let key = world.shared.config.definition(SPEED).unwrap();
    assert_eq!(key.description, "how fast");
    assert_eq!(world.shared.config.get(SPEED), Some(Value::Int(9)));
    assert_eq!(speed(&world).await, Ok(9));

    world.shared.config.set(SPEED, Value::Int(7)).unwrap();
    assert!(
        heard(&world, "speed 7", 1).await,
        "{:?}",
        announced(&world).await
    );
    assert_eq!(speed(&world).await, Ok(7));

    let tuned = host(&world)
        .command(Player::new(77, "Steve"), "tune".into())
        .await
        .unwrap();
    assert_eq!(tuned.result, Ok(0));
    assert_eq!(world.shared.config.get(SPEED), Some(Value::Int(42)));
    assert!(heard(&world, "speed 42", 1).await);
    assert_eq!(
        world.shared.config.set(SPEED, Value::Bool(true)),
        Err(crate::config::ConfigError::WrongKind {
            key: SPEED.into(),
            expected: crate::config::Kind::Int
        })
    );

    console(&world, "tuff plugin unload tuner").await;
    assert_eq!(world.shared.config.get(SPEED), None);
    assert!(
        world
            .shared
            .config
            .saved_text()
            .contains(&(SPEED.to_owned(), "42".to_owned()))
    );
    console(&world, "tuff plugin load tuner").await;
    assert_eq!(speed(&world).await, Ok(42));
    world.shared.config.reset(SPEED).unwrap();
    assert!(heard(&world, "speed 1", 1).await);
    assert_eq!(count(&world, "speed 42").await, 1, "a reload is no change");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_resource_packs_are_hosted_while_the_plugin_is_loaded() {
    let dir = TestDir::new();
    install(dir.path(), "tuner.wasm", &tuner());
    let world = start(dir.path()).await;
    let packs = world.shared.resource_packs.list().await;
    let ours: Vec<_> = packs.iter().filter(|p| p.name == "tuner:look").collect();
    assert_eq!(ours.len(), 1, "{packs:?}");
    let id = ours[0].id;
    let built = world.shared.resource_packs.fetch(id).await.unwrap();
    assert_eq!(built.bytes, pack_zip());
    let offer = world.shared.resource_packs.offer(id).await.unwrap();
    assert!(!offer.required);

    console(&world, "tuff plugin unload tuner").await;
    assert!(
        !world
            .shared
            .resource_packs
            .list()
            .await
            .iter()
            .any(|p| p.id == id)
    );
    console(&world, "tuff plugin load tuner").await;
    let again = world.shared.resource_packs.list().await;
    assert!(again.iter().any(|p| p.id == id), "the id is stable");
    world.shutdown().await;
}
