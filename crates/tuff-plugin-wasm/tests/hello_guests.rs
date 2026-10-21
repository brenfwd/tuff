//! The `hello` example plugin, which `tuff plugin new` scaffolds, written
//! on each SDK's high-level layer: `examples/rust-hello` (Rust) and
//! `examples/cpp-hello` (C++). Both must behave identically, down to the
//! bytes of every message and stored value, which is what keeps the two
//! SDKs' surfaces in step. Each is skipped when it is not built, unless
//! `TUFF_EXAMPLE_PLUGINS=1` requires it.

use std::path::{Path, PathBuf};

use tuff_plugin_wasm::sandbox::{Effects, Limits, Player, Sandbox, engine};
use tuff_plugin_wasm::wit::bindings::types::{
    BlockChange, BlockPos, ConfigChange, ConfigValue, Event, EventKinds, Invocation,
};

/// The executor's timer tokens start here (bit 30), in both SDKs.
const SDK_TIMERS: u32 = 0x4000_0000;

fn built(relative: &str) -> Option<Vec<u8>> {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(relative);
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

fn alex() -> Player {
    Player::new(0xa1e8, "Alex")
}

fn op() -> Player {
    let mut op = Player::new(0x0b, "Op");
    op.level = tuff_storage::admin::PermissionLevel::from_id(2);
    op
}

fn text(parts: &[&str]) -> String {
    parts.concat().replace('&', "\u{a7}")
}

fn broken(player: Option<&Player>, state: &str, dimension: &str) -> Event {
    Event::BlockBreak(BlockChange {
        player: player.map(Player::to_wit),
        pos: BlockPos { x: 1, y: 12, z: 3 },
        dimension: dimension.into(),
        state: state.into(),
    })
}

fn one_sdk_timer(effects: &Effects, delay: u64) -> u32 {
    let [(after, token)] = effects.timers[..] else {
        panic!("one timer: {effects:?}");
    };
    assert_eq!(after, delay);
    assert!(token >= SDK_TIMERS, "token {token:#x} is the executor's");
    token
}

fn behaves_like_hello(wasm: &[u8]) {
    let manifest = tuff_plugin_wasm::manifest::read(wasm).unwrap();
    assert_eq!(
        (
            manifest.id.as_str(),
            manifest.api,
            manifest.description.as_str()
        ),
        ("hello", 0, "Greets players and counts their visits")
    );
    assert_eq!(manifest.version.to_string(), "0.1.0");
    let mut plugin = Sandbox::new(&engine(), wasm, "hello", Limits::default()).unwrap();

    let registration = plugin.register().unwrap();
    assert_eq!(
        registration.events,
        EventKinds::CONFIG_CHANGE | EventKinds::JOIN | EventKinds::BLOCK_BREAK
    );
    let [setting] = &registration.settings[..] else {
        panic!("one setting: {registration:?}");
    };
    assert_eq!(
        (setting.key.as_str(), setting.description.as_str()),
        ("greeting", "What /hello says")
    );
    assert_eq!(
        setting.default,
        tuff_core::config::Value::String("Hello".into())
    );
    let [permission] = &registration.permissions[..] else {
        panic!("one permission: {registration:?}");
    };
    assert_eq!(
        (permission.node.as_str(), permission.level),
        ("hello.reset", 2)
    );
    let [tree] = &registration.commands[..] else {
        panic!("one command: {registration:?}");
    };
    let nodes: Vec<(&str, Option<u32>, Option<&str>)> = tree
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n.executes, n.permission.as_deref()))
        .collect();
    assert_eq!(
        nodes,
        [
            ("hello", Some(0), None),
            ("later", Some(1), None),
            ("reset", Some(2), Some("hello.reset")),
            ("help", Some(3), None),
        ]
    );

    let enabled = plugin.enable().unwrap();
    let tip = one_sdk_timer(&enabled, 6000);

    for visit in 1..=2 {
        let (result, effects) = plugin.command(alex(), "hello").unwrap();
        assert_eq!(result, Ok(visit));
        let expected = Effects {
            replies: vec![text(&[
                "&6Hello&r, &bAlex&r! &7(visit ",
                &visit.to_string(),
                ")&r",
            ])],
            stored: vec![(
                "visits/0000000000000000000000000000a1e8".into(),
                Some(visit.to_string().into_bytes()),
            )],
            ..Effects::default()
        };
        assert_eq!(effects, expected);
    }

    let (result, effects) = plugin.command(alex(), "hello help").unwrap();
    assert_eq!(result, Ok(2));
    assert_eq!(
        effects.replies,
        [
            text(&["&6/hello&r - Greets you"]),
            text(&["&6/hello later&r - Greets you again in two seconds"]),
        ]
    );
    let console = Invocation {
        handler: 3,
        sender: None,
        args: Vec::new(),
        line: "hello help".into(),
    };
    let (result, _) = plugin.invoke(None, &console).unwrap();
    assert_eq!(result, Ok(3));

    assert!(plugin.command(alex(), "hello reset").unwrap().0.is_err());
    let (result, effects) = plugin.command(op(), "hello reset").unwrap();
    assert_eq!(result, Ok(1));
    assert_eq!(effects.replies, ["Your visits are forgotten"]);
    assert_eq!(
        effects.stored,
        [("visits/0000000000000000000000000000000b".to_owned(), None)]
    );

    let (result, effects) = plugin.command(alex(), "hello later").unwrap();
    assert_eq!(result, Ok(1));
    assert_eq!(effects.replies, ["See you in two seconds"]);
    let later = one_sdk_timer(&effects, 40);
    assert_eq!(
        plugin.timer(later).unwrap(),
        Effects {
            tells: vec![(0xa1e8, text(&["&6Hello&r again, &bAlex&r!"]))],
            ..Effects::default()
        }
    );

    let tipped = plugin.timer(tip).unwrap();
    assert_eq!(tipped.broadcasts, [text(&["&aTip: &rsay hi with /hello"])]);
    one_sdk_timer(&tipped, 6000);

    assert_eq!(
        plugin
            .event(&Event::Join(alex().to_wit()), Some(alex()))
            .unwrap(),
        Effects {
            tells: vec![(0xa1e8, text(&["Welcome! Say hi with &6/hello&r"]))],
            ..Effects::default()
        }
    );

    let overworld = "minecraft:overworld";
    let quiet = [
        broken(Some(&alex()), "minecraft:stone", overworld),
        broken(
            Some(&alex()),
            "minecraft:diamond_ore",
            "minecraft:the_nether",
        ),
        broken(None, "minecraft:diamond_ore", overworld),
    ];
    for event in &quiet {
        assert_eq!(plugin.event(event, None).unwrap(), Effects::default());
    }
    let found = plugin
        .event(
            &broken(Some(&alex()), "minecraft:diamond_ore", overworld),
            Some(alex()),
        )
        .unwrap();
    assert_eq!(
        found.broadcasts,
        [text(&["&bAlex&r found &b&ldiamonds&r!"])]
    );

    let change = Event::ConfigChange(ConfigChange {
        value: ConfigValue::Text("Howdy".into()),
        key: "greeting".into(),
    });
    assert_eq!(
        plugin.event(&change, None).unwrap().broadcasts,
        [text(&["The greeting is now &6Howdy&r"])]
    );
}

#[tuff_core::watchdog]
#[test]
fn rust_hello_behaves_like_hello() {
    if let Some(wasm) = built("rust-hello/target/wasm32-unknown-unknown/release/hello.wasm") {
        behaves_like_hello(&wasm);
    }
}

#[tuff_core::watchdog]
#[test]
fn cpp_hello_behaves_like_hello() {
    if let Some(wasm) = built("cpp-hello/build/hello.wasm") {
        behaves_like_hello(&wasm);
    }
}
