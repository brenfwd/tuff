use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::auth::offline_uuid;
use tuff_core::server::ServerHandle;
use tuff_storage::admin::format_uuid;
use tuff_vanilla_data::packet::configuration::Clientbound as Config;

fn server(instance: &TempDir) -> Server {
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(
        tuff_feature_builtin_vanilla_commands::debugconfig::DebugConfigFeature,
    ));
    Server::builder()
        .features(features)
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build()
}

async fn listed(handle: &ServerHandle, name: &str) -> bool {
    handle
        .run_command("list")
        .await
        .unwrap()
        .iter()
        .any(|line| line.plain().contains(name))
}

async fn wait_listed(handle: &ServerHandle, name: &str, wanted: bool) {
    while listed(handle, name).await != wanted {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn count(ids: &[i32], packet: Config) -> usize {
    ids.iter().filter(|id| **id == packet.id()).count()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn debugconfig_holds_a_player_in_configuration_and_returns_them() {
    let instance = TempDir::new();
    let server = server(&instance);
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let bot = Bot::connect(addr, BotConfig::new("Configured"))
        .await
        .unwrap();
    wait_listed(&handle, "Configured", true).await;
    let uuid = format_uuid(offline_uuid("Configured"));
    let first = bot.state().configuration;
    let teleports = bot.state().teleports;

    let missing = handle
        .run_command(&format!("debugconfig unconfig {uuid}"))
        .await
        .unwrap();
    assert_eq!(missing[0].plain(), "Can't find player to unconfig");

    let switched = handle
        .run_command("debugconfig config Configured")
        .await
        .unwrap();
    assert_eq!(
        switched[0].plain(),
        format!("Switched player Configured({uuid}) to config mode")
    );
    bot.wait_for(|state| state.configuring).await.unwrap();
    wait_listed(&handle, "Configured", false).await;

    let released = handle
        .run_command(&format!("debugconfig unconfig {uuid}"))
        .await
        .unwrap();
    assert!(released.is_empty(), "{:?}", released[0].plain());
    bot.wait_for(|state| state.reconfigurations == 1 && state.teleports > teleports)
        .await
        .unwrap();
    wait_listed(&handle, "Configured", true).await;

    let all = bot.state().configuration;
    let again = &all[first.len()..];
    assert_eq!(
        count(again, Config::RegistryData),
        count(&first, Config::RegistryData)
    );
    assert_eq!(count(again, Config::UpdateTags), 1);
    assert_eq!(count(again, Config::FinishConfiguration), 1);
    assert!(bot.state().disconnected.is_none());

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconfiguration_without_hold_resyncs_and_rejoins_at_once() {
    use tuff_core::world::messages::PhaseRequest;
    let instance = TempDir::new();
    let server = server(&instance);
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let bot = Bot::connect(addr, BotConfig::new("Resynced"))
        .await
        .unwrap();
    wait_listed(&handle, "Resynced", true).await;
    let teleports = bot.state().teleports;
    let shared = handle.world_shared().await.unwrap();
    let uuid = offline_uuid("Resynced");

    assert!(
        !shared
            .request_phase(uuid, |reply| PhaseRequest::Release { reply })
            .await
    );
    assert!(
        shared
            .request_phase(uuid, |reply| PhaseRequest::Configure { hold: false, reply })
            .await
    );
    bot.wait_for(|state| state.reconfigurations == 1 && state.teleports > teleports)
        .await
        .unwrap();
    wait_listed(&handle, "Resynced", true).await;
    assert!(bot.state().disconnected.is_none());

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
