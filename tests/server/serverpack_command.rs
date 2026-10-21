use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::auth::name_uuid;
use tuff_vanilla_data::packet::play::Clientbound;

const URL: &str = "http://127.0.0.1:1/pack.zip";

fn server(instance: &TempDir) -> Server {
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(
        tuff_feature_builtin_vanilla_commands::serverpack::ServerPackFeature,
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

#[tuff_core::watchdog]
#[test]
fn name_uuids_follow_the_version_three_layout() {
    assert_eq!(name_uuid(b"hello"), 0x5d41402a_bc4b_3a76_b971_9d911017c592);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serverpack_pushes_and_pops_packs_for_every_player() {
    let instance = TempDir::new();
    let server = server(&instance);
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let bot = Bot::connect(addr, BotConfig::new("Packed")).await.unwrap();
    // The bot is placed before the coordinator lists it as a target.
    while !handle
        .run_command("list")
        .await
        .unwrap()
        .iter()
        .any(|line| line.plain().contains("Packed"))
    {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let first = handle
        .run_command(&format!("serverpack push \"{URL}\""))
        .await
        .unwrap();
    assert!(first.is_empty(), "{:?}", first[0].to_value());
    bot.wait_for(|state| state.pack_offers.iter().any(|o| o.in_play))
        .await
        .unwrap();
    let derived = bot.state().pack_offers.pop().unwrap();
    assert_eq!(derived.url, URL);
    assert_eq!(derived.id, name_uuid(URL.as_bytes()));
    assert_eq!(derived.hash, "");
    assert!(!derived.required);

    handle
        .run_command(&format!(
            "serverpack push \"{URL}\" 00000000-0000-0000-0000-00000000abcd 0123456789abcdef0123456789abcdef01234567"
        ))
        .await
        .unwrap();
    bot.wait_for(|state| state.pack_offers.iter().filter(|o| o.in_play).count() == 2)
        .await
        .unwrap();
    let explicit = bot.state().pack_offers.pop().unwrap();
    assert_eq!(explicit.id, 0xabcd);
    assert_eq!(explicit.hash, "0123456789abcdef0123456789abcdef01234567");

    handle
        .run_command("serverpack pop 00000000-0000-0000-0000-00000000abcd")
        .await
        .unwrap();
    let pop = Clientbound::ResourcePackPop.id();
    bot.wait_for(|state| state.received.get(&pop).copied().unwrap_or(0) == 1)
        .await
        .unwrap();

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
