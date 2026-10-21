use std::path::PathBuf;

use tuff_core::config::ConfigStore;
use tuff_storage::backend::WorldBackend;
use tuff_storage::sqlite::SqliteBackend;

use super::*;
use crate::manifest;
use crate::wit::bindings::types::{
    AssetDecl, AssetFile, DataFile as WitFile, EntityDecl, ItemDecl, PartDecl,
};
use crate::wit::canon::Value;
use crate::wit::testing::{self, Data, export};
use crate::wit::value::ToValue;

const MANIFEST: &str = "id = gems\nversion = 1.0.0\napi = 0\ncap.content = mine, other -- gems\n";

fn file(id: &str, json: &str) -> WitFile {
    WitFile {
        id: id.into(),
        json: json.into(),
    }
}

fn decl() -> ContentDecl {
    let item = |id: &str, base: &str, components: &str| ItemDecl {
        id: id.into(),
        base: base.into(),
        model: Some("mine:ruby".into()),
        components: components.into(),
    };
    ContentDecl {
        items: vec![
            item("mine:ruby", "emerald", "{\"minecraft:max_stack_size\":16}"),
            item("other:sapphire", "minecraft:diamond", ""),
            item("mine:nothing", "minecraft:no_such_item", ""),
            item("mine:broken", "stick", "{oops"),
        ],
        entities: vec![EntityDecl {
            id: "mine:golem".into(),
            stand_in: "iron_golem".into(),
            parts: vec![
                PartDecl {
                    kind: "item_display".into(),
                    data: "{item_display:\"head\"}".into(),
                },
                PartDecl {
                    kind: "interaction".into(),
                    data: String::new(),
                },
            ],
        }],
        assets: vec![AssetDecl {
            name: "textures".into(),
            namespace: "mine".into(),
            files: vec![AssetFile {
                path: "textures/item/ruby.png".into(),
                data: vec![1, 2, 3],
            }],
        }],
        recipes: vec![file("mine:ruby", "{}"), file("minecraft:stick", "{}")],
        advancements: vec![file("mine:found", "{}")],
    }
}

/// A module declaring [`decl`]; with `trap`, `declare-content` calls an
/// import first, which traps.
fn module(trap: bool) -> Vec<u8> {
    let mut data = Data::new();
    let declared = data.export_result("declare-content", &decl().to_value());
    let log = data.call("log", &[Value::Variant(0, None), Value::String("x".into())]);
    let call = if trap { log } else { String::new() };
    let body = format!(
        "(func {declare} {call} (i32.const {declared}))
         {data}",
        declare = export("declare-content"),
        data = data.wat(),
    );
    let mut wasm = wat::parse_str(testing::module(&body)).unwrap();
    wasm.extend(manifest::custom_section(
        manifest::SECTION,
        MANIFEST.as_bytes(),
    ));
    wasm
}

struct Setup {
    dir: PathBuf,
    _backend: SqliteBackend,
    ctx: ProviderContext,
}

fn setup(name: &str, wasm: &[u8], grant: Option<&str>) -> Setup {
    let dir = std::env::temp_dir().join(format!("tuff-content-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("plugins")).unwrap();
    std::fs::write(dir.join("plugins").join("gems.wasm"), wasm).unwrap();
    let backend = SqliteBackend::open(dir.join("server.db")).unwrap();
    let store = backend.config().expect("sqlite keeps the config");
    if let Some(grant) = grant {
        store.write_plugin_grant("gems", Some(grant)).unwrap();
    }
    let settings = ConfigStore::new();
    for key in crate::config_keys() {
        settings.register(key).unwrap();
    }
    Setup {
        ctx: ProviderContext {
            config_store: store,
            settings,
            properties: Default::default(),
        },
        dir,
        _backend: backend,
    }
}

fn declared(setup: &Setup) -> Vec<ProvidedContent> {
    let provider = WasmContent::new(Some(setup.dir.join("plugins")), Limits::default());
    provider.attach(&setup.ctx);
    provider.content()
}

#[tuff_core::watchdog]
#[test]
fn granted_content_is_converted_and_the_rest_left_out() {
    let s = setup("granted", &module(false), Some("content = mine"));
    let declared = declared(&s);
    assert_eq!(declared.len(), 1);
    let content = &declared[0];
    assert_eq!(content.owner, "gems");
    let items: Vec<_> = content.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(items, ["mine:ruby"]);
    let ruby = &content.items[0];
    assert_eq!(
        (ruby.base, ruby.model.as_deref()),
        (Item::Emerald, Some("mine:ruby"))
    );
    assert_eq!(ruby.stack(1).max_stack_size(), 16);
    assert_eq!(content.entities[0].stand_in.name(), "minecraft:iron_golem");
    assert_eq!(content.entities[0].parts.len(), 2);
    assert_eq!(content.assets[0].0, "textures");
    assert_eq!(content.assets[0].1.files[0].1.as_ref(), [1, 2, 3]);
    let recipes: Vec<_> = content.recipes.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(recipes, ["mine:ruby"]);
    assert_eq!(content.advancements[0].id, "mine:found");
}

#[tuff_core::watchdog]
#[test]
fn nothing_is_declared_without_a_grant_or_from_a_trapping_module() {
    let s = setup("ungranted", &module(false), None);
    assert!(declared(&s).is_empty());
    let s = setup("trapping", &module(true), Some("content = mine"));
    assert!(declared(&s).is_empty());
}

#[tuff_core::watchdog]
#[test]
fn a_module_without_the_export_declares_nothing() {
    let body = format!("(func {})", export("on-timer"));
    let mut wasm = wat::parse_str(testing::module(&body)).unwrap();
    wasm.extend(manifest::custom_section(
        manifest::SECTION,
        MANIFEST.as_bytes(),
    ));
    let s = setup("plain", &wasm, Some("content = mine"));
    assert!(declared(&s).is_empty());
}

#[tuff_core::watchdog]
#[test]
fn a_cpp_plugin_declares_content_through_the_sdk_builders() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = std::env::temp_dir().join(format!("tuff-cpp-content-{}.wasm", std::process::id()));
    let built = std::process::Command::new(root.join("../tuff-plugin-sdk/c/build.sh"))
        .arg(&out)
        .arg(root.join("tests/guests/cpp-content/content.cpp"))
        .output();
    let wasm = match built {
        Ok(done) if done.status.success() => {
            let wasm = std::fs::read(&out).unwrap();
            let _ = std::fs::remove_file(&out);
            wasm
        }
        other => {
            if std::env::var("TUFF_EXAMPLE_PLUGINS").as_deref() == Ok("1") {
                panic!("cpp-content did not build: {other:?}");
            }
            eprintln!("skipping: cpp-content did not build");
            return;
        }
    };
    let s = setup("cpp", &wasm, Some("content = example"));
    let content = declared(&s);
    assert_eq!(content.len(), 1);
    let content = &content[0];
    assert_eq!(content.items.len(), 1);
    assert_eq!(content.items[0].id, "example:ruby");
    assert_eq!(content.entities.len(), 1);
    assert_eq!(content.assets.len(), 1);
    assert_eq!(content.recipes[0].id, "example:ruby_recipe");
    assert_eq!(content.advancements[0].id, "example:got_ruby");
}
