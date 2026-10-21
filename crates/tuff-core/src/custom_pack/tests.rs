use super::*;
use crate::protocol::pack::PackStatus;
use crate::resource_pack::PackEvent;
use crate::world::messages::{PlayerEvent, PlayerHandle};
use sha1::Sha1;

fn set(namespace: &str, files: &[(&str, &str)]) -> AssetSet {
    AssetSet {
        namespace: namespace.to_owned(),
        files: files
            .iter()
            .map(|(p, b)| ((*p).to_owned(), Bytes::from(b.as_bytes().to_vec())))
            .collect(),
    }
}

fn entry(key: &str, feature: &'static str, set: AssetSet) -> AssetEntry {
    AssetEntry {
        key: key.to_owned(),
        feature,
        set: Arc::new(set),
    }
}

fn json(bytes: &Bytes) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

fn sha1_hex(bytes: &[u8]) -> String {
    Sha1::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn pack_id_is_a_stable_name_uuid() {
    assert_eq!(pack_id(), pack_id());
    let bytes = pack_id().to_be_bytes();
    assert_eq!(bytes[6] >> 4, 3);
    assert_eq!(bytes[8] >> 6, 0b10);
}

#[tuff_core::watchdog]
#[test]
fn mergeable_paths() {
    for path in [
        "assets/minecraft/sounds.json",
        "assets/ex/lang/en_us.json",
        "assets/minecraft/atlases/blocks.json",
        "assets/ex/font/include/space.json",
        "assets/minecraft/blockstates/note_block.json",
    ] {
        assert!(is_mergeable(path), "{path}");
    }
    for path in [
        "assets/ex/items/ruby.json",
        "assets/ex/textures/item/ruby.png",
        "assets/ex/font/glyphs.ttf",
        "assets/ex/sounds/ding.ogg",
        "pack.mcmeta",
    ] {
        assert!(!is_mergeable(path), "{path}");
    }
}

#[tuff_core::watchdog]
#[test]
fn distinct_files_keep_their_bytes() {
    let files = merge(&[
        entry("a:1", "a", set("a", &[("items/x.json", "{ }")])),
        entry("b:1", "b", set("b", &[("items/x.json", "[1]")])),
    ])
    .unwrap();
    assert_eq!(files["assets/a/items/x.json"].as_ref(), b"{ }");
    assert_eq!(files["assets/b/items/x.json"].as_ref(), b"[1]");
}

#[tuff_core::watchdog]
#[test]
fn plain_collisions_name_both_sets() {
    let err = merge(&[
        entry(
            "a:tex",
            "a",
            set("minecraft", &[("textures/block/stone.png", "A")]),
        ),
        entry(
            "b:tex",
            "b",
            set("minecraft", &[("textures/block/stone.png", "A")]),
        ),
    ])
    .unwrap_err();
    assert_eq!(err.path, "assets/minecraft/textures/block/stone.png");
    assert_eq!(err.first, ("a:tex".to_owned(), "a"));
    assert_eq!(err.second, ("b:tex".to_owned(), "b"));
    let text = err.to_string();
    assert!(
        text.contains("feature a") && text.contains("feature b"),
        "{text}"
    );
}

#[tuff_core::watchdog]
#[test]
fn shared_json_merges_key_by_key() {
    let files = merge(&[
        entry(
            "a:lang",
            "a",
            set(
                "minecraft",
                &[
                    ("lang/en_us.json", r#"{"item.a.one":"One","shared":"Same"}"#),
                    (
                        "atlases/blocks.json",
                        r#"{"sources":[{"type":"directory","source":"a"}]}"#,
                    ),
                    ("sounds.json", r#"{"a.ding":{"sounds":["a:ding"]}}"#),
                ],
            ),
        ),
        entry(
            "b:lang",
            "b",
            set(
                "minecraft",
                &[
                    ("lang/en_us.json", r#"{"item.b.two":"Two","shared":"Same"}"#),
                    (
                        "atlases/blocks.json",
                        r#"{"sources":[{"type":"directory","source":"b"}]}"#,
                    ),
                    ("sounds.json", r#"{"b.ding":{"sounds":["b:ding"]}}"#),
                ],
            ),
        ),
    ])
    .unwrap();
    assert_eq!(
        json(&files["assets/minecraft/lang/en_us.json"]),
        serde_json::json!({"item.a.one":"One","shared":"Same","item.b.two":"Two"})
    );
    assert_eq!(
        json(&files["assets/minecraft/atlases/blocks.json"]),
        serde_json::json!({"sources":[
            {"type":"directory","source":"a"},
            {"type":"directory","source":"b"}
        ]})
    );
    let sounds = json(&files["assets/minecraft/sounds.json"]);
    assert!(sounds.get("a.ding").is_some() && sounds.get("b.ding").is_some());
}

#[tuff_core::watchdog]
#[test]
fn blockstate_variants_merge_and_clash_per_state() {
    let variants =
        |state: &str, model: &str| format!(r#"{{"variants":{{"{state}":{{"model":"{model}"}}}}}}"#);
    let one = variants("instrument=bit,note=1,powered=true", "a:block/ore");
    let two = variants("instrument=bit,note=2,powered=true", "b:block/lamp");
    let path = "blockstates/note_block.json";
    let files = merge(&[
        entry("a:b", "a", set("minecraft", &[(path, &one)])),
        entry("b:b", "b", set("minecraft", &[(path, &two)])),
    ])
    .unwrap();
    let merged = json(&files["assets/minecraft/blockstates/note_block.json"]);
    assert_eq!(merged["variants"].as_object().unwrap().len(), 2);

    let clash = variants("instrument=bit,note=1,powered=true", "c:block/other");
    let err = merge(&[
        entry("a:b", "a", set("minecraft", &[(path, &one)])),
        entry("b:b", "b", set("minecraft", &[(path, &two)])),
        entry("c:b", "c", set("minecraft", &[(path, &clash)])),
    ])
    .unwrap_err();
    assert_eq!(err.first.1, "a");
    assert_eq!(err.second.1, "c");
    assert!(
        err.detail
            .contains("/variants/instrument=bit,note=1,powered=true/model"),
        "{}",
        err.detail
    );
}

#[tuff_core::watchdog]
#[test]
fn shared_files_must_be_json_objects() {
    let lang = "lang/en_us.json";
    let err = merge(&[
        entry("a:l", "a", set("ex", &[(lang, r#"{"k":"v"}"#)])),
        entry("b:l", "b", set("ex", &[(lang, "not json")])),
    ])
    .unwrap_err();
    assert_eq!(err.second.1, "b");
    let err = merge(&[
        entry("a:l", "a", set("ex", &[(lang, "[]")])),
        entry("b:l", "b", set("ex", &[(lang, r#"{"k":"v"}"#)])),
    ])
    .unwrap_err();
    assert_eq!((err.first.1, err.second.1), ("b", "a"));
    let err = merge(&[
        entry("a:l", "a", set("ex", &[(lang, r#"{"k":"v"}"#)])),
        entry("b:l", "b", set("ex", &[(lang, r#"{"k":"w"}"#)])),
    ])
    .unwrap_err();
    assert_eq!((err.first.1, err.second.1), ("a", "b"));
    assert_eq!(err.detail, "differs at /k");
}

#[tuff_core::watchdog]
#[test]
fn pack_declares_the_running_format_and_is_deterministic() {
    let entries = vec![
        entry(
            "a:1",
            "a",
            set("a", &[("items/x.json", "{}"), ("models/x.json", "{}")]),
        ),
        entry(
            "b:1",
            "b",
            set("minecraft", &[("lang/en_us.json", r#"{"k":"v"}"#)]),
        ),
    ];
    let files = merge(&entries).unwrap();
    let zip_bytes = build_pack(&files).unwrap();
    assert_eq!(zip_bytes, build_pack(&merge(&entries).unwrap()).unwrap());
    let read = zip::read(&zip_bytes, zip::Limits::default()).unwrap();
    let names: Vec<_> = read.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "assets/a/items/x.json",
            "assets/a/models/x.json",
            "assets/minecraft/lang/en_us.json",
            "pack.mcmeta"
        ]
    );
    let meta: Value = serde_json::from_slice(&read[3].1).unwrap();
    let version = tuff_vanilla_data::version::RESOURCE_PACK_VERSION;
    let format = serde_json::json!([version.major, version.minor]);
    assert_eq!(meta["pack"]["min_format"], format);
    assert_eq!(meta["pack"]["max_format"], format);
    assert!(meta["pack"]["description"].is_string());
    assert!(meta["pack"].get("pack_format").is_none());

    let mut other = entries.clone();
    other[0] = entry("a:1", "a", set("a", &[("items/x.json", "{\"changed\":1}")]));
    assert_ne!(build_pack(&merge(&other).unwrap()).unwrap(), zip_bytes);
}

fn packs() -> ResourcePacks {
    ResourcePacks::spawn(
        crate::public_ip::BaseUrl::detached("http://203.0.113.5:8080"),
        ConfigStore::new(),
    )
}

async fn start(
    packs: &ResourcePacks,
    config: &ConfigStore,
    initial: Vec<AssetEntry>,
) -> CustomPack {
    CustomPack::start(packs.clone(), config.clone(), initial).await
}

async fn served(packs: &ResourcePacks) -> Option<Vec<(String, Bytes)>> {
    let built = packs.fetch(pack_id()).await?;
    Some(zip::read(&built.bytes, zip::Limits::default()).unwrap())
}

fn ruby() -> Vec<AssetEntry> {
    vec![entry(
        "ex:main",
        "ex",
        set("ex", &[("items/ruby.json", "{}")]),
    )]
}

async fn next_event(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> PlayerEvent {
    tokio::time::timeout(std::time::Duration::from_secs(10), events.recv())
        .await
        .expect("an event arrives")
        .expect("the handle is alive")
}

#[tuff_core::watchdog]
#[tokio::test]
async fn startup_assets_are_offered_under_the_stable_id() {
    let packs = packs();
    let config = ConfigStore::new();
    let pack = start(&packs, &config, ruby()).await;
    let offer = packs.offer(pack_id()).await.unwrap();
    let expected = build_pack(&merge(&ruby()).unwrap()).unwrap();
    assert_eq!(offer.sha1, sha1_hex(&expected));
    assert!(offer.required);
    assert!(offer.prompt.is_none());
    assert_eq!(packs.join_offers().await, vec![offer]);
    let files = served(&packs).await.unwrap();
    assert!(files.iter().any(|(n, _)| n == "assets/ex/items/ruby.json"));
    assert_eq!(pack.files().await.len(), 1);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn nothing_is_hosted_without_assets() {
    let packs = packs();
    let config = ConfigStore::new();
    let pack = start(&packs, &config, Vec::new()).await;
    assert!(packs.offer(pack_id()).await.is_none());
    assert!(packs.join_offers().await.is_empty());
    assert_eq!(pack.remove_assets("ex", "ex:none").await, Ok(false));

    let (handle, mut events) = PlayerHandle::new(7);
    packs.player_joined(7, handle).await;
    assert_eq!(
        pack.set_assets("ex", "ex:late", set("ex", &[("items/a.json", "{}")]))
            .await,
        Ok(true)
    );
    let PlayerEvent::ResourcePack(offer) = next_event(&mut events).await else {
        panic!("expected a pack push");
    };
    assert_eq!(offer.id, pack_id());
    assert!(served(&packs).await.is_some());

    assert_eq!(pack.remove_assets("ex", "ex:late").await, Ok(true));
    assert!(packs.offer(pack_id()).await.is_none());
    let popped = next_event(&mut events).await;
    assert!(matches!(popped, PlayerEvent::ResourcePackPop(Some(id)) if id == pack_id()));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn runtime_update_repushes_new_hash_to_holders() {
    let packs = packs();
    let config = ConfigStore::new();
    let pack = start(&packs, &config, ruby()).await;
    let first = packs.offer(pack_id()).await.unwrap();

    let (holder, mut holder_events) = PlayerHandle::new(1);
    let (decliner, mut decliner_events) = PlayerHandle::new(2);
    packs
        .report_join(
            PackEvent {
                player: 1,
                pack: pack_id(),
                status: PackStatus::Loaded,
            },
            &first.sha1,
        )
        .await;
    packs.player_joined(1, holder).await;
    packs
        .report_join(
            PackEvent {
                player: 2,
                pack: pack_id(),
                status: PackStatus::Declined,
            },
            &first.sha1,
        )
        .await;
    let states = packs.player_joined(2, decliner).await;
    assert_eq!(states[&pack_id()].status, Some(PackStatus::Declined));
    assert_eq!(states[&pack_id()].hash, first.sha1);

    let changed = set("ex", &[("items/ruby.json", "{\"v\":2}")]);
    assert_eq!(
        pack.set_assets("ex", "ex:main", changed.clone()).await,
        Ok(true)
    );
    let PlayerEvent::ResourcePack(offer) = next_event(&mut holder_events).await else {
        panic!("expected a pack push");
    };
    assert_eq!(offer.id, first.id);
    assert_eq!(offer.url, first.url);
    assert_ne!(offer.sha1, first.sha1);
    assert_eq!(packs.offer(pack_id()).await.unwrap().sha1, offer.sha1);
    assert!(
        decliner_events.try_recv().is_err(),
        "a decliner is not re-prompted"
    );
    assert_eq!(pack.set_assets("ex", "ex:main", changed).await, Ok(false));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn conflicting_update_keeps_the_previous_pack() {
    let packs = packs();
    let config = ConfigStore::new();
    let initial = vec![entry("a:main", "a", set("a", &[("items/x.json", "{}")]))];
    let pack = start(&packs, &config, initial).await;
    let before = packs.offer(pack_id()).await.unwrap().sha1;

    let err = pack
        .set_assets("b", "b:clash", set("a", &[("items/x.json", "{}")]))
        .await
        .unwrap_err();
    let UpdateError::Conflict(conflict) = err else {
        panic!("expected a conflict, got {err:?}");
    };
    assert_eq!(conflict.first, ("a:main".to_owned(), "a"));
    assert_eq!(conflict.second, ("b:clash".to_owned(), "b"));
    assert_eq!(
        pack.set_assets("b", "a:main", set("b", &[("items/y.json", "{}")]))
            .await,
        Err(UpdateError::NotOwner {
            key: "a:main".into(),
            owner: "a"
        })
    );
    assert!(matches!(
        pack.set_assets("a", "a:bad", set("a", &[("../x.json", "{}")]))
            .await,
        Err(UpdateError::Invalid(_))
    ));
    assert_eq!(packs.offer(pack_id()).await.unwrap().sha1, before);
    assert_eq!(pack.files().await.len(), 1);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn pack_is_always_required_and_prompt_applies_live() {
    let packs = packs();
    let config = ConfigStore::new();
    config
        .set(
            PROMPT_KEY,
            crate::config::Value::String("Get the pack".into()),
        )
        .unwrap();
    let pack = start(&packs, &config, ruby()).await;
    let offer = packs.offer(pack_id()).await.unwrap();
    assert!(offer.required);
    assert_eq!(offer.prompt.unwrap().plain(), "Get the pack");
    assert!(
        config
            .definition("tuff.customContent.requirePack")
            .is_none()
    );

    config
        .set(PROMPT_KEY, crate::config::Value::String("New text".into()))
        .unwrap();
    let mut prompt = String::new();
    for _ in 0..1000 {
        pack.files().await;
        let offer = packs.offer(pack_id()).await.unwrap();
        assert!(offer.required);
        prompt = offer.prompt.map(|p| p.plain()).unwrap_or_default();
        if prompt == "New text" {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(prompt, "New text");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn pack_is_offered_with_feature_packs_switched_off() {
    let config = ConfigStore::new();
    config
        .set(
            crate::resource_pack::ENABLED_KEY,
            crate::config::Value::Bool(false),
        )
        .unwrap();
    let packs = ResourcePacks::spawn(
        crate::public_ip::BaseUrl::detached("http://203.0.113.5:8080"),
        config.clone(),
    );
    packs
        .register(ResourcePack::new(
            1,
            "feature",
            PackSource::Bytes(Bytes::from_static(b"x")),
        ))
        .await;
    start(&packs, &config, ruby()).await;
    let offers = packs.join_offers().await;
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].id, pack_id());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn joining_with_a_stale_or_missing_answer_gets_the_current_pack() {
    let packs = packs();
    let config = ConfigStore::new();
    let pack = start(&packs, &config, Vec::new()).await;

    let (missed, mut missed_events) = PlayerHandle::new(1);
    assert_eq!(
        pack.set_assets("ex", "ex:main", set("ex", &[("items/ruby.json", "{}")]))
            .await,
        Ok(true)
    );
    let first = packs.offer(pack_id()).await.unwrap();
    packs.player_joined(1, missed).await;
    let PlayerEvent::ResourcePack(offer) = next_event(&mut missed_events).await else {
        panic!("expected a pack push");
    };
    assert_eq!(offer.sha1, first.sha1);
    assert!(offer.required);

    let answer = |player| PackEvent {
        player,
        pack: pack_id(),
        status: PackStatus::Loaded,
    };
    packs.report_join(answer(2), &first.sha1).await;
    assert_eq!(
        pack.set_assets(
            "ex",
            "ex:main",
            set("ex", &[("items/ruby.json", "{\"v\":2}")])
        )
        .await,
        Ok(true)
    );
    let (stale, mut stale_events) = PlayerHandle::new(2);
    packs.player_joined(2, stale).await;
    let PlayerEvent::ResourcePack(offer) = next_event(&mut stale_events).await else {
        panic!("expected a pack push");
    };
    let current = packs.offer(pack_id()).await.unwrap();
    assert_ne!(current.sha1, first.sha1);
    assert_eq!(offer.sha1, current.sha1);

    packs.report_join(answer(3), &current.sha1).await;
    let (fresh, mut fresh_events) = PlayerHandle::new(3);
    packs.player_joined(3, fresh).await;
    packs.offer(pack_id()).await.unwrap();
    assert!(
        fresh_events.try_recv().is_err(),
        "an up-to-date player is not re-prompted"
    );
}

#[tuff_core::watchdog]
#[test]
fn pack_state_tracks_loads_across_versions() {
    let mut state = crate::resource_pack::PackState::default();
    state.offered("aa");
    assert!(!state.loaded);
    state.apply(PackStatus::Accepted);
    state.apply(PackStatus::Loaded);
    assert!(state.loaded);
    state.offered("bb");
    assert_eq!(state.status, None);
    assert!(state.loaded, "the old version stays applied until replaced");
    state.apply(PackStatus::FailedDownload);
    assert!(!state.loaded);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_panic_restarts_the_actor_with_its_assets() {
    let packs = packs();
    let config = ConfigStore::new();
    let pack = start(&packs, &config, ruby()).await;
    pack.panic_for_test().await;
    assert_eq!(pack.files().await.len(), 1);
}
