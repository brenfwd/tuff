use crate::common;

use common::{MockSessionServer, start_server};

async fn query(server: &common::TestServer, what: &str) -> i64 {
    let lines = server
        .handle
        .run_command(&format!("time query {what}"))
        .await
        .unwrap();
    lines[0].to_value()["with"][if what == "time" { 1 } else { 0 }]["text"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn run(server: &common::TestServer, line: &str) {
    server.handle.run_command(line).await.unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn advance_time_off_freezes_the_day_but_not_game_time_or_commands() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    run(&server, "time set 1000").await;
    run(&server, "gamerule advance_time false").await;
    let day = query(&server, "time").await;
    let game = query(&server, "gametime").await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while query(&server, "gametime").await < game + 5 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("game time keeps running");
    assert_eq!(query(&server, "time").await, day);

    run(&server, "time add 100").await;
    assert_eq!(query(&server, "time").await, day + 100);

    run(&server, "gamerule advance_time true").await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while query(&server, "time").await < day + 105 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("day resumes");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}
