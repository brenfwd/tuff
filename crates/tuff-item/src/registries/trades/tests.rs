use std::sync::Arc;

use serde_json::json;
use tuff_vanilla_data::data_registry::DataRegistry;

use super::*;
use crate::registries::{Loader, Registries};

fn decode_with<C: RegistryCodec>(id: &str, json: &Value) -> Result<C::Value, String> {
    C::decode(
        json,
        &DecodeCtx {
            registries: &Registries::vanilla(),
            id,
        },
    )
}

#[tuff_core::watchdog]
#[test]
fn vanilla_trade_json_decodes_to_the_generated_trades() {
    assert!(!generated::TRADE_JSON.is_empty());
    for (name, raw) in generated::TRADE_JSON {
        let json: Value = serde_json::from_str(raw).unwrap();
        let decoded = decode_with::<TradeCodec>(name, &json).unwrap();
        assert_eq!(&decoded, Trade::builtin(name).unwrap(), "{name}");
    }
}

#[tuff_core::watchdog]
#[test]
fn vanilla_trade_set_json_decodes_to_the_generated_sets() {
    assert!(!generated::TRADE_SET_JSON.is_empty());
    for (name, raw) in generated::TRADE_SET_JSON {
        let json: Value = serde_json::from_str(raw).unwrap();
        let decoded = decode_with::<TradeSetCodec>(name, &json).unwrap();
        assert_eq!(&decoded, TradeSet::builtin(name).unwrap(), "{name}");
    }
}

#[tuff_core::watchdog]
#[test]
fn unsupported_trade_fields_are_rejected() {
    let base = json!({"wants": {"id": "minecraft:emerald"}, "gives": {"id": "minecraft:bread"}});
    assert!(decode_with::<TradeCodec>("a:b", &base).is_ok());
    let mut bad = base.clone();
    bad["nonsense"] = json!(1);
    assert!(decode_with::<TradeCodec>("a:b", &bad).is_err());
    let mut bad = base.clone();
    bad["gives"]["id"] = json!("minecraft:not_an_item");
    assert!(decode_with::<TradeCodec>("a:b", &bad).is_err());
    let mut bad = base;
    bad["given_item_modifier"] = json!({"function": "x", "type": "minecraft:set_name"});
    assert!(decode_with::<TradeCodec>("a:b", &bad).is_err());
    let set = json!({"trades": ["minecraft:not_a_trade"], "amount": 1});
    assert!(decode_with::<TradeSetCodec>("a:b", &set).is_err());
}

fn registry(key: &str) -> &'static DataRegistry {
    DataRegistry::get(key).unwrap()
}

#[tuff_core::watchdog]
#[test]
fn a_pack_trade_set_and_trade_replace_the_built_in_ones() {
    let mut loader = Loader::default();
    loader.insert(
        registry(TradeCodec::KEY),
        "example:golden_bread".to_owned(),
        "file/example",
        br#"{"wants": {"id": "minecraft:emerald", "count": 3}, "gives": {"id": "minecraft:golden_carrot", "count": 2}, "max_uses": 7, "xp": 9}"#.to_vec(),
    );
    loader.insert(
        registry(TradeSetCodec::KEY),
        "minecraft:farmer/level_1".to_owned(),
        "file/example",
        br#"{"trades": ["example:golden_bread"], "amount": 2}"#.to_vec(),
    );
    let registries = loader.freeze().unwrap();
    let trades = registries.trades();
    assert!(trades.sets.modified());
    let set = trades.sets.get("minecraft:farmer/level_1").unwrap();
    let drawn = trades.of_set(set);
    assert_eq!(drawn.len(), 1);
    assert_eq!(drawn[0].gives, Item::GoldenCarrot);
    assert_eq!(drawn[0].wants.count, 3);
    assert!(
        trades
            .sets
            .entry("minecraft:farmer/level_2")
            .unwrap()
            .is_builtin()
    );
    assert!(Arc::ptr_eq(&Registries::vanilla(), &Registries::vanilla()));
}

#[tuff_core::watchdog]
#[test]
fn a_trade_set_naming_a_missing_trade_rejects_the_load() {
    let mut loader = Loader::default();
    loader.insert(
        registry(TradeSetCodec::KEY),
        "minecraft:farmer/level_1".to_owned(),
        "file/example",
        br#"{"trades": ["example:missing"], "amount": 2}"#.to_vec(),
    );
    let error = loader.freeze().err().unwrap();
    assert_eq!(error.registry, TradeSetCodec::KEY);
}
