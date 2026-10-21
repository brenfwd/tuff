use super::*;

const ENCHANTMENT: &str = "minecraft:enchantment";

/// The enchantment registry as if clients were not sent it, so entries
/// without a network codec can be added.
fn enchantments() -> &'static DataRegistry {
    static UNSYNCED: OnceLock<DataRegistry> = OnceLock::new();
    UNSYNCED.get_or_init(|| DataRegistry {
        synchronized: false,
        ..*DataRegistry::get(ENCHANTMENT).unwrap()
    })
}

/// A copy of the enchantment registry whose built-in `sharpness` file is
/// `bytes`.
fn with_builtin_sharpness(bytes: &[u8]) -> &'static DataRegistry {
    let real = enchantments();
    let mut hashes = real.hashes.to_vec();
    let i = real.entries.binary_search(&"minecraft:sharpness").unwrap();
    hashes[i] = short_sha1(bytes);
    Box::leak(Box::new(DataRegistry {
        hashes: Box::leak(hashes.into_boxed_slice()),
        ..*real
    }))
}

struct Level;

impl RegistryCodec for Level {
    const KEY: &'static str = ENCHANTMENT;
    type Value = u32;

    fn builtin(_: &str) -> Option<&'static u32> {
        Some(&0)
    }

    fn decode(json: &serde_json::Value, _: &DecodeCtx<'_>) -> Result<u32, String> {
        json["max_level"]
            .as_u64()
            .map(|level| level as u32)
            .ok_or_else(|| "missing max_level".to_owned())
    }
}

#[tuff_core::watchdog]
#[test]
fn every_built_in_entry_has_a_hash() {
    for registry in data_registry::ALL {
        assert_eq!(
            registry.entries.len(),
            registry.hashes.len(),
            "{}",
            registry.key
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn no_pack_files_share_the_vanilla_snapshot() {
    let frozen = Loader::default().freeze().unwrap();
    assert!(Arc::ptr_eq(&frozen, &Registries::vanilla()));
    assert!(!frozen.modified());
    assert!(frozen.iter().all(|registry| !registry.modified()));
    let registry = frozen.get(ENCHANTMENT).unwrap();
    assert_eq!(registry.len(), enchantments().entries.len());
    for (i, id) in enchantments().entries.iter().enumerate() {
        assert_eq!(registry.index_of(id), Some(i as u32));
        assert!(registry.entry(id).unwrap().is_builtin());
    }
}

#[tuff_core::watchdog]
#[test]
fn a_hidden_built_in_entry_keeps_its_slot_but_is_not_found() {
    let banner = DataRegistry::get("minecraft:banner_pattern").unwrap();
    let mut loader = Loader::default();
    loader.hide(banner, "minecraft:creeper");
    loader.insert(
        banner,
        "a:zap".into(),
        "p",
        br#"{"asset_id":"a:zap","translation_key":"zap"}"#.to_vec(),
    );
    let frozen = loader.freeze().unwrap();
    let patterns = frozen.get("minecraft:banner_pattern").unwrap();
    assert!(patterns.modified());
    assert!(!patterns.contains("minecraft:creeper"));
    assert_eq!(
        frozen.exists("minecraft:banner_pattern", "minecraft:creeper"),
        Some(false)
    );
    let creeper = banner.entries.binary_search(&"minecraft:creeper").unwrap();
    assert_eq!(patterns.id(creeper as u32), None);
    assert_eq!(patterns.len(), banner.entries.len() + 1);
    assert!(patterns.iter().all(|(id, _)| id != "minecraft:creeper"));
    let slots: Vec<(&str, bool)> = patterns.network_iter().map(|(id, _, h)| (id, h)).collect();
    assert_eq!(slots[creeper], ("minecraft:creeper", true));
    assert_eq!(slots.last(), Some(&("a:zap", false)));
    for (index, id) in banner.entries.iter().enumerate() {
        if *id != "minecraft:creeper" {
            assert_eq!(patterns.index_of(id), Some(index as u32));
        }
    }

    let mut loader = Loader::default();
    loader.hide(enchantments(), "minecraft:sharpness");
    loader.insert(
        enchantments(),
        "minecraft:sharpness".into(),
        "p",
        br#"{"max_level":3}"#.to_vec(),
    );
    let frozen = loader.freeze().unwrap();
    assert!(frozen.exists(ENCHANTMENT, "minecraft:sharpness").unwrap());
}

#[tuff_core::watchdog]
#[test]
fn packs_add_and_override_entries_and_the_last_wins() {
    let mut loader = Loader::default();
    let registry = enchantments();
    loader.insert(
        registry,
        "minecraft:sharpness".into(),
        "low",
        br#"{"max_level":9}"#.to_vec(),
    );
    loader.insert(
        registry,
        "minecraft:sharpness".into(),
        "high",
        br#"{"max_level":7}"#.to_vec(),
    );
    loader.insert(
        registry,
        "a:zap".into(),
        "low",
        br#"{"max_level":2}"#.to_vec(),
    );
    loader.insert(
        registry,
        "b:zap".into(),
        "low",
        br#"{"max_level":3}"#.to_vec(),
    );
    let frozen = loader.freeze().unwrap();
    assert!(frozen.modified());
    let enchantment = frozen.get(ENCHANTMENT).unwrap();
    assert!(enchantment.modified());
    assert!(!frozen.get("minecraft:painting_variant").unwrap().modified());
    assert_eq!(enchantment.len(), registry.entries.len() + 2);
    let sharpness = enchantment.entry("minecraft:sharpness").unwrap();
    assert_eq!(sharpness.value().pack.as_deref(), Some("high"));
    assert!(
        enchantment
            .entry("minecraft:protection")
            .unwrap()
            .is_builtin()
    );
    let a = enchantment.index_of("a:zap").unwrap();
    assert_eq!(enchantment.index_of("b:zap"), Some(a + 1));
    assert_eq!(a as usize, registry.entries.len());
    let levels = frozen.decode::<Level>().unwrap();
    assert_eq!(levels.get("minecraft:sharpness"), Some(&7));
    assert_eq!(levels.get("b:zap"), Some(&3));
    assert_eq!(levels.get("minecraft:protection"), Some(&0));
    assert_eq!(frozen.exists(ENCHANTMENT, "a:zap"), Some(true));
    assert_eq!(frozen.exists("minecraft:item", "a:zap"), None);
}

#[tuff_core::watchdog]
#[test]
fn a_file_identical_to_the_built_in_one_stays_built_in() {
    let vanilla = br#"{"max_level":5}"#;
    let registry = with_builtin_sharpness(vanilla);
    let mut loader = Loader::default();
    loader.insert(
        registry,
        "minecraft:sharpness".into(),
        "low",
        br#"{"max_level":1}"#.to_vec(),
    );
    loader.insert(
        registry,
        "minecraft:sharpness".into(),
        "high",
        vanilla.to_vec(),
    );
    let frozen = loader.freeze().unwrap();
    assert!(Arc::ptr_eq(&frozen, &Registries::vanilla()));
}

#[tuff_core::watchdog]
#[test]
fn a_bad_file_rejects_the_whole_set() {
    let mut loader = Loader::default();
    loader.insert(
        enchantments(),
        "a:good".into(),
        "fine",
        br#"{"max_level":1}"#.to_vec(),
    );
    loader.insert(
        enchantments(),
        "a:bad".into(),
        "broken",
        b"{not json".to_vec(),
    );
    let err = loader.freeze().err().unwrap();
    assert_eq!(err.pack, "broken");
    assert_eq!(err.id, "a:bad");
    assert_eq!(err.registry, ENCHANTMENT);

    let mut loader = Loader::default();
    loader.insert(enchantments(), "a:list".into(), "p", b"[]".to_vec());
    assert!(loader.freeze().is_err());

    let mut loader = Loader::default();
    loader.insert(
        enchantments(),
        "a:odd".into(),
        "p",
        br#"{"max_level":"x"}"#.to_vec(),
    );
    let err = loader.freeze().unwrap().decode::<Level>().err().unwrap();
    assert_eq!((err.pack.as_str(), err.id.as_str()), ("p", "a:odd"));
}

#[tuff_core::watchdog]
#[test]
fn paths_map_to_the_longest_registry_directory() {
    let (registry, id) = classify("worldgen/placed_feature/a/b.json").unwrap();
    assert_eq!(
        (registry.key, id),
        ("minecraft:worldgen/placed_feature", "a/b")
    );
    let (registry, id) = classify("enchantment/zap.json").unwrap();
    assert_eq!((registry.key, id), (ENCHANTMENT, "zap"));
    assert!(classify("recipe/x.json").is_none());
    assert!(classify("enchantment/zap.txt").is_none());
}

#[tuff_core::watchdog]
#[test]
fn every_synchronized_registry_sends_pack_entries_checked_against_built_in_fields() {
    let banner = DataRegistry::get("minecraft:banner_pattern").unwrap();
    let mut loader = Loader::default();
    loader.insert(banner, "a:zap".into(), "p", br#"{"max_level":2}"#.to_vec());
    let err = loader.freeze().err().unwrap();
    assert_eq!((err.pack.as_str(), err.id.as_str()), ("p", "a:zap"));
    assert!(err.message.contains("missing"), "{}", err.message);

    let mut loader = Loader::default();
    loader.insert(
        banner,
        "a:zap".into(),
        "p",
        br#"{"asset_id":"a:zap","translation_key":7}"#.to_vec(),
    );
    assert!(
        loader
            .freeze()
            .err()
            .unwrap()
            .message
            .contains("wrong type")
    );

    let mut loader = Loader::default();
    loader.insert(
        banner,
        "a:zap".into(),
        "p",
        br#"{"asset_id":"a:zap","translation_key":"zap"}"#.to_vec(),
    );
    let chat = DataRegistry::get("minecraft:chat_type").unwrap();
    loader.insert(
        chat,
        "a:shout".into(),
        "p",
        br#"{"chat":{"translation_key":"%s!","parameters":["content"]},"narration":{"translation_key":"%s","parameters":["content"]}}"#
            .to_vec(),
    );
    let instance = DataRegistry::get("minecraft:test_instance").unwrap();
    loader.insert(
        instance,
        "a:check".into(),
        "p",
        br#"{"type":"minecraft:function","environment":"minecraft:default","function":"a:f","max_ticks":5,"structure":"minecraft:empty"}"#
            .to_vec(),
    );
    let mut bad = Loader::default();
    bad.insert(
        instance,
        "a:check".into(),
        "p",
        br#"{"type":"minecraft:function","environment":"a:nowhere","function":"a:f","max_ticks":5,"structure":"minecraft:empty"}"#
            .to_vec(),
    );
    assert!(
        bad.freeze()
            .err()
            .unwrap()
            .message
            .contains("unknown test environment")
    );
    let frozen = loader.freeze().unwrap();
    for (key, id) in [
        ("minecraft:banner_pattern", "a:zap"),
        ("minecraft:chat_type", "a:shout"),
        ("minecraft:test_instance", "a:check"),
    ] {
        let entry = frozen.get(key).unwrap().entry(id).unwrap();
        assert!(entry.value().network.get().is_some(), "{key}");
    }
}

#[tuff_core::watchdog]
#[test]
fn every_built_in_entry_passes_its_registry_field_check() {
    for registry in tuff_vanilla_data::data_registry::SYNCHRONIZED {
        let fields = DataRegistry::required_fields(registry.key);
        if registry.key != "minecraft:world_clock" && !registry.list_entries() {
            assert!(!fields.is_empty(), "{}", registry.key);
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn pack_damage_types_and_paintings_get_network_nbt() {
    let damage = DataRegistry::get("minecraft:damage_type").unwrap();
    let painting = DataRegistry::get("minecraft:painting_variant").unwrap();
    let mut loader = Loader::default();
    loader.insert(
        damage,
        "minecraft:arrow".into(),
        "p",
        br#"{"message_id":"zapped","scaling":"never","exhaustion":0.5,"effects":"burning"}"#
            .to_vec(),
    );
    loader.insert(
        painting,
        "a:mural".into(),
        "p",
        br#"{"width":2,"height":3,"asset_id":"a:mural","title":{"text":"Mural"}}"#.to_vec(),
    );
    loader.insert(painting, "a:bad".into(), "q", br#"{"width":0}"#.to_vec());
    let err = loader.freeze().err().unwrap();
    assert_eq!((err.pack.as_str(), err.id.as_str()), ("q", "a:bad"));

    let mut loader = Loader::default();
    loader.insert(
        damage,
        "minecraft:arrow".into(),
        "p",
        br#"{"message_id":"zapped","scaling":"never","exhaustion":0.5,"effects":"burning"}"#
            .to_vec(),
    );
    loader.insert(
        painting,
        "a:mural".into(),
        "p",
        br#"{"width":2,"height":3,"asset_id":"a:mural","title":{"text":"Mural"}}"#.to_vec(),
    );
    let frozen = loader.freeze().unwrap();
    let read = |key: &str, id: &str| {
        let bytes = frozen
            .get(key)
            .unwrap()
            .entry(id)
            .unwrap()
            .value()
            .network
            .get()
            .unwrap()
            .clone();
        let tag = tuff_nbt::Tag::read_network(&mut bytes.as_slice(), tuff_nbt::NbtLimits::NETWORK)
            .unwrap()
            .unwrap();
        let tuff_nbt::Tag::Compound(compound) = tag else {
            panic!("not a compound")
        };
        compound
    };
    let arrow = read("minecraft:damage_type", "minecraft:arrow");
    assert_eq!(
        arrow.get("message_id"),
        Some(&tuff_nbt::Tag::String("zapped".into()))
    );
    assert_eq!(
        arrow.get("scaling"),
        Some(&tuff_nbt::Tag::String("never".into()))
    );
    assert_eq!(arrow.get("exhaustion"), Some(&tuff_nbt::Tag::Float(0.5)));
    assert_eq!(
        arrow.get("effects"),
        Some(&tuff_nbt::Tag::String("burning".into()))
    );
    assert!(arrow.get("death_message_type").is_none());
    let mural = read("minecraft:painting_variant", "a:mural");
    assert_eq!(mural.get("width"), Some(&tuff_nbt::Tag::Int(2)));
    assert_eq!(
        mural.get("asset_id"),
        Some(&tuff_nbt::Tag::String("a:mural".into()))
    );
    assert!(mural.get("title").is_some());
    let paintings = frozen.get("minecraft:painting_variant").unwrap();
    assert_eq!(paintings.len(), painting.entries.len() + 1);
    assert_eq!(
        paintings.index_of("a:mural"),
        Some(painting.entries.len() as u32)
    );
    assert!(
        frozen
            .get("minecraft:damage_type")
            .unwrap()
            .entry("minecraft:cactus")
            .unwrap()
            .is_builtin()
    );
    let builtin = codecs::DamageTypes::builtin("minecraft:arrow").unwrap();
    assert_eq!(builtin.message_id, "arrow");
}
