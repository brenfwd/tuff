//! The `wgreet` example plugin, built by each language's toolchain on its
//! SDK: `crates/tuff-plugin-wasm/tests/guests/rust-wit-greeter` (Rust), `crates/tuff-plugin-wasm/tests/guests/c-wit-greeter` (C),
//! `crates/tuff-plugin-wasm/tests/guests/cpp-greeter` (C++).
//! Each is skipped when it is not built, unless `TUFF_EXAMPLE_PLUGINS=1`
//! requires it.

use std::path::{Path, PathBuf};

use tuff_plugin_wasm::sandbox::{Effects, Limits, Player, Sandbox, engine};
use tuff_plugin_wasm::wit::bindings::types::{
    Arg, ArgValue, CommandNode, DoubleRange, Event, EventKinds, IntRange, Invocation, NodeKind,
    Parser,
};

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/guests")
}

fn built(relative: &str) -> Option<Vec<u8>> {
    let path = examples().join(relative);
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

fn newest_modified(path: &Path) -> Option<std::time::SystemTime> {
    let meta = std::fs::metadata(path).ok()?;
    let own = meta.modified().ok();
    if !meta.is_dir() {
        return own;
    }
    std::fs::read_dir(path)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_name() != "build")
        .filter_map(|entry| newest_modified(&entry.path()))
        .chain(own)
        .max()
}

fn rebuild_c_guest_when_stale() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let example = root.join("crates/tuff-plugin-wasm/tests/guests/c-wit-greeter");
    let wasm = example.join("build/wit-greeter.wasm");
    let Some(built_at) = std::fs::metadata(&wasm).and_then(|m| m.modified()).ok() else {
        return;
    };
    let stale = [
        example.clone(),
        root.join("crates/tuff-plugin-sdk/c"),
        root.join("crates/tuff-plugin-wasm/wit"),
    ]
    .iter()
    .filter_map(|dir| newest_modified(dir))
    .any(|changed| changed > built_at);
    if !stale {
        return;
    }
    let rebuilt = std::process::Command::new("sh")
        .arg(example.join("build.sh"))
        .status()
        .is_ok_and(|status| status.success());
    if !rebuilt {
        let _ = std::fs::remove_file(&wasm);
        eprintln!("c-wit-greeter is stale and could not be rebuilt");
    }
}

fn player(name: &str) -> Player {
    Player::new(0xfeed_beef_0000_0000_0000_0000_0000_0001, name)
}

fn call(handler: u32, sender: Option<&Player>, args: Vec<(&str, ArgValue)>) -> Invocation {
    Invocation {
        handler,
        sender: sender.map(Player::to_wit),
        args: args
            .into_iter()
            .map(|(name, value)| Arg {
                name: name.into(),
                value,
            })
            .collect(),
        line: "wgreet".into(),
    }
}

fn replies(texts: &[&str]) -> Effects {
    Effects {
        replies: texts.iter().map(|t| (*t).into()).collect(),
        ..Effects::default()
    }
}

fn broadcast(text: &str) -> Effects {
    Effects {
        broadcasts: vec![text.into()],
        ..Effects::default()
    }
}

/// Both guests implement the same plugin; this checks it.
fn behaves_like_the_greeter(wasm: &[u8]) {
    let manifest = tuff_plugin_wasm::manifest::read(wasm).unwrap();
    assert_eq!((manifest.id.as_str(), manifest.api), ("wgreet", 0));
    assert!(manifest.description.starts_with("Greets players"));
    let mut plugin = Sandbox::new(&engine(), wasm, "wgreet", Limits::default()).unwrap();
    let registration = plugin.register().unwrap();
    assert_eq!(registration.events, EventKinds::JOIN | EventKinds::LEAVE);
    let [tree] = &registration.commands[..] else {
        panic!("one command: {registration:?}");
    };
    let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "wgreet", "target", "times", "sum", "a", "b", "volume", "level"
        ]
    );
    assert_eq!(tree.nodes[0].children, [1, 3, 6]);
    assert_eq!(
        tree.nodes[2].kind,
        NodeKind::Argument(Parser::Integer(IntRange {
            min: Some(1),
            max: Some(3)
        }))
    );
    assert_eq!(
        tree.nodes[7],
        CommandNode {
            name: "level".into(),
            kind: NodeKind::Argument(Parser::Double(DoubleRange {
                min: Some(0.0),
                max: Some(1.0)
            })),
            children: vec![],
            executes: Some(4),
            redirect: None,
            permission: None,
            suggest: true,
        }
    );
    assert_eq!(tree.nodes[6].permission.as_deref(), Some("wgreet.volume"));

    let mut op = player("Alex");
    op.level = tuff_storage::admin::PermissionLevel::from_id(4);
    assert_eq!(
        plugin.suggest(op.clone(), "wgreet volume 0.").unwrap(),
        ["0.5"]
    );
    assert_eq!(
        plugin.suggest(op, "wgreet volume ").unwrap(),
        ["0", "0.5", "1"]
    );

    let alex = player("Alex");
    let mut run = |invocation: Invocation| {
        let sender = invocation.sender.as_ref().map(|_| alex.clone());
        plugin.invoke(sender, &invocation).unwrap()
    };
    assert_eq!(
        run(call(0, Some(&alex), vec![])),
        (Ok(1), replies(&["Hello, Alex!"]))
    );
    assert_eq!(run(call(0, None, vec![])).0, Err("players only".into()));
    let target = ("target", ArgValue::Player(player("Steve").to_wit()));
    assert_eq!(
        run(call(
            2,
            Some(&alex),
            vec![target.clone(), ("times", ArgValue::Integer(3))]
        )),
        (Ok(3), replies(&["Hello, Steve!"; 3]))
    );
    assert_eq!(
        run(call(
            3,
            Some(&alex),
            vec![("a", ArgValue::Integer(-40)), ("b", ArgValue::Integer(2))]
        )),
        (Ok(-38), replies(&["-40 + 2 = -38"]))
    );
    assert_eq!(
        run(call(
            4,
            Some(&alex),
            vec![("level", ArgValue::Double(0.25))]
        )),
        (Ok(1), replies(&["volume 25%"]))
    );
    assert!(run(call(9, Some(&alex), vec![])).0.is_err());
    for _ in 0..2000 {
        assert_eq!(run(call(1, Some(&alex), vec![target.clone()])).0, Ok(1));
    }

    let (result, effects) = plugin.command(alex.clone(), "wgreet sum 4 5").unwrap();
    assert_eq!((result, effects), (Ok(9), replies(&["4 + 5 = 9"])));

    assert_eq!(
        plugin
            .event(&Event::Join(alex.to_wit()), Some(alex.clone()))
            .unwrap(),
        broadcast("Alex joined")
    );
    assert_eq!(
        plugin
            .event(&Event::Leave(alex.to_wit()), Some(alex))
            .unwrap(),
        broadcast("Alex left")
    );
}

#[tuff_core::watchdog]
#[test]
fn rust_guest_behaves_like_the_greeter() {
    if let Some(wasm) =
        built("rust-wit-greeter/target/wasm32-unknown-unknown/release/wit_greeter.wasm")
    {
        behaves_like_the_greeter(&wasm);
    }
}

#[tuff_core::watchdog]
#[test]
fn c_guest_behaves_like_the_greeter() {
    rebuild_c_guest_when_stale();
    if let Some(wasm) = built("c-wit-greeter/build/wit-greeter.wasm") {
        behaves_like_the_greeter(&wasm);
    }
}

#[tuff_core::watchdog]
#[test]
fn cpp_guest_behaves_like_the_greeter() {
    if let Some(wasm) = built("cpp-greeter/build/wit-greeter.wasm") {
        behaves_like_the_greeter(&wasm);
    }
}

#[tuff_core::watchdog]
#[test]
fn cpp_raw_binding_guest_behaves_like_the_greeter() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/guests/cpp-raw-greeter/build/wit-greeter.wasm");
    match std::fs::read(&path) {
        Ok(wasm) => behaves_like_the_greeter(&wasm),
        Err(_) if std::env::var("TUFF_EXAMPLE_PLUGINS").as_deref() == Ok("1") => {
            panic!("{} is not built", path.display())
        }
        Err(_) => eprintln!("skipping: {} is not built", path.display()),
    }
}
