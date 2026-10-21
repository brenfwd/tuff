use crate::common;

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use common::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::custom::AssetSet;
use tuff_core::custom_pack::{UpdateError, pack_id, zip};
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::protocol::pack::PackStatus;
use tuff_core::resource_pack::{ROUTE_PREFIX, format_uuid};
use tuff_item::load::{LoadError, Phase};

const PLUGIN: &str = "custom-pack-test";

fn gem_set(model: &str) -> AssetSet {
    AssetSet {
        namespace: "cp".into(),
        files: vec![
            (
                "items/gem.json".into(),
                Bytes::from(format!(
                    r#"{{"model":{{"type":"minecraft:model","model":"{model}"}}}}"#
                )),
            ),
            (
                "lang/en_us.json".into(),
                Bytes::from_static(br#"{"item.cp.gem":"Gem"}"#),
            ),
        ],
    }
}

struct AssetFeature;

impl Feature for AssetFeature {
    fn id(&self) -> &'static str {
        PLUGIN
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            let custom = &mut cx.registries().custom;
            custom.register_assets(PLUGIN, "cp:gem", gem_set("minecraft:item/emerald"))?;
            custom.register_assets(
                PLUGIN,
                "cp:names",
                AssetSet {
                    namespace: "minecraft".into(),
                    files: vec![(
                        "lang/en_us.json".into(),
                        Bytes::from_static(br#"{"block.minecraft.note_block":"Odd Block"}"#),
                    )],
                },
            )?;
        }
        Ok(())
    }
}

async fn get(addr: SocketAddr, path: &str) -> Vec<u8> {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut out))
        .await
        .unwrap()
        .unwrap();
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("http response has a body");
    assert!(
        out.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&out)
    );
    out[split + 4..].to_vec()
}

async fn served_hash(handle: &tuff::ServerHandle, body: &[u8]) -> String {
    let built = handle
        .resource_packs()
        .await
        .unwrap()
        .fetch(pack_id())
        .await
        .expect("the custom pack is hosted");
    assert_eq!(
        built.bytes.as_ref(),
        body,
        "served bytes are the built pack"
    );
    built.sha1.clone()
}

#[tuff_core::watchdog]
#[tokio::test]
async fn generated_pack_is_offered_served_and_repushed() {
    let instance = TempDir::new();
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(AssetFeature));
    let server = Server::builder()
        .features(features)
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let http = handle.http_addrs().await.unwrap()[0];
    let http = SocketAddr::new([127, 0, 0, 1].into(), http.port());

    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            pack_status: PackStatus::Loaded,
            ..BotConfig::new("Packer")
        },
    )
    .await
    .unwrap();
    let joined = bot.state().pack_offers;
    let offer = joined
        .iter()
        .find(|o| o.id == pack_id())
        .expect("custom pack offered in configuration")
        .clone();
    assert!(!offer.in_play);
    assert!(offer.required);
    let path = format!("{ROUTE_PREFIX}{}.zip", format_uuid(pack_id()));
    assert!(offer.url.ends_with(&path), "{}", offer.url);

    let body = get(http, &path).await;
    assert_eq!(served_hash(&handle, &body).await, offer.hash);
    let files = zip::read(&body, zip::Limits::default()).unwrap();
    let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "assets/cp/items/gem.json",
            "assets/cp/lang/en_us.json",
            "assets/minecraft/lang/en_us.json",
            "pack.mcmeta"
        ]
    );

    for refusal in [PackStatus::Declined, PackStatus::FailedDownload] {
        let refused = Bot::connect(
            addr,
            BotConfig {
                view_distance: 2,
                pack_status: refusal,
                ..BotConfig::new("Refuser")
            },
        )
        .await;
        let err = refused.err().expect("a client without the pack is kicked");
        assert!(
            format!("{err:#}").contains("during configuration"),
            "{refusal:?}: {err:#}"
        );
    }
    let dropper = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            pack_status: PackStatus::Loaded,
            play_pack_status: Some(PackStatus::Declined),
            ..BotConfig::new("Dropper")
        },
    )
    .await
    .unwrap();

    let custom = handle.custom_pack().await.unwrap();
    assert_eq!(
        custom
            .set_assets(PLUGIN, "cp:gem", gem_set("minecraft:item/diamond"))
            .await,
        Ok(true)
    );
    bot.wait_for(|state| state.pack_offers.iter().any(|o| o.in_play))
        .await
        .unwrap();
    let repushed = bot
        .state()
        .pack_offers
        .into_iter()
        .find(|o| o.in_play)
        .unwrap();
    assert_eq!(repushed.id, pack_id());
    assert_eq!(repushed.url, offer.url);
    assert_ne!(repushed.hash, offer.hash);
    assert!(repushed.required);
    dropper
        .wait_for(|state| state.disconnected.is_some())
        .await
        .unwrap();
    assert!(
        dropper
            .state()
            .disconnected
            .as_deref()
            .is_some_and(|r| r.starts_with("kicked"))
    );
    assert!(bot.state().disconnected.is_none());
    let body = get(http, &path).await;
    assert_eq!(served_hash(&handle, &body).await, repushed.hash);
    let files = zip::read(&body, zip::Limits::default()).unwrap();
    let gem = &files
        .iter()
        .find(|(n, _)| n == "assets/cp/items/gem.json")
        .unwrap()
        .1;
    assert!(String::from_utf8_lossy(gem).contains("minecraft:item/diamond"));

    let clash = AssetSet {
        namespace: "cp".into(),
        files: vec![("items/gem.json".into(), Bytes::from_static(b"{}"))],
    };
    assert!(matches!(
        custom.set_assets("other-plugin", "other:gem", clash).await,
        Err(UpdateError::Conflict(_))
    ));
    let body = get(http, &path).await;
    assert_eq!(served_hash(&handle, &body).await, repushed.hash);

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn no_pack_is_asked_for_without_custom_content() {
    let instance = TempDir::new();
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];

    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            pack_status: PackStatus::Declined,
            ..BotConfig::new("Plain")
        },
    )
    .await
    .expect("a client declining every pack still joins");
    assert!(bot.state().pack_offers.is_empty());
    assert!(
        handle
            .resource_packs()
            .await
            .unwrap()
            .fetch(pack_id())
            .await
            .is_none()
    );
    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
