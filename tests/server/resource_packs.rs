use crate::common;

use bytes::Bytes;
use common::TempDir;
use tuff::Server;
use tuff::protocol::Encode;
use tuff_bot::{Bot, BotConfig};
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::protocol::pack::PackStatus;
use tuff_core::resource_pack::{PackSource, ResourcePack};
use tuff_item::load::{LoadError, Phase};

const REQUIRED: u128 = 0x11;
const OPTIONAL: u128 = 0x22;

struct PackFeature {
    required: bool,
    on_join: bool,
}

impl Feature for PackFeature {
    fn id(&self) -> &'static str {
        "resource-pack-test"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            let id = if self.required { REQUIRED } else { OPTIONAL };
            cx.registries().resource_packs.push(
                ResourcePack::new(id, "test", PackSource::Bytes(Bytes::from_static(b"zip")))
                    .required(self.required)
                    .offer_on_join(self.on_join),
            );
        }
        Ok(())
    }
}

async fn start(
    required: bool,
    on_join: bool,
) -> (
    tuff::ServerHandle,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    std::net::SocketAddr,
    TempDir,
) {
    let instance = TempDir::new();
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(PackFeature { required, on_join }));
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
    (handle, task, addr, instance)
}

fn config(name: &str, status: PackStatus) -> BotConfig {
    BotConfig {
        view_distance: 2,
        pack_status: status,
        ..BotConfig::new(name)
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn required_pack_declined_disconnects_and_loaded_joins() {
    let (handle, task, addr, _dir) = start(true, true).await;
    assert!(
        Bot::connect(addr, config("Decliner", PackStatus::Declined))
            .await
            .is_err()
    );
    assert!(
        Bot::connect(addr, config("Failer", PackStatus::FailedDownload))
            .await
            .is_err()
    );
    let bot = Bot::connect(addr, config("Accepter", PackStatus::Loaded))
        .await
        .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn optional_pack_declined_still_joins() {
    let (handle, task, addr, _dir) = start(false, true).await;
    let bot = Bot::connect(addr, config("Decliner", PackStatus::Declined))
        .await
        .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    assert!(bot.state().disconnected.is_none());
    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

async fn pushed(bot: &Bot, handle: &tuff::ServerHandle, id: u128) {
    let packs = handle.resource_packs().await.unwrap();
    for _ in 0..200 {
        packs.push(None, id).await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if bot.state().disconnected.is_some()
            || bot
                .state()
                .received
                .contains_key(&tuff_core::protocol::pack::PlayPush::ID)
        {
            return;
        }
    }
    panic!("pack never reached the player");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn play_push_reports_status_and_kicks_on_required_failure() {
    let (handle, task, addr, _dir) = start(true, false).await;
    let packs = handle.resource_packs().await.unwrap();
    let mut events = packs.subscribe();
    let bot = Bot::connect(addr, config("Late", PackStatus::Loaded))
        .await
        .unwrap();
    assert!(
        bot.state().pack_offers.is_empty(),
        "no custom content, no generated pack"
    );
    pushed(&bot, &handle, REQUIRED).await;
    let mut statuses = Vec::new();
    while statuses.last() != Some(&PackStatus::Loaded) {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.pack, REQUIRED);
        statuses.push(event.status);
    }
    assert_eq!(
        statuses,
        [
            PackStatus::Accepted,
            PackStatus::Downloaded,
            PackStatus::Loaded
        ]
    );
    assert!(bot.state().disconnected.is_none());
    drop(bot);

    let bot = Bot::connect(addr, config("Refuser", PackStatus::Declined))
        .await
        .unwrap();
    pushed(&bot, &handle, REQUIRED).await;
    bot.wait_for(|state| state.disconnected.is_some())
        .await
        .unwrap();
    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
