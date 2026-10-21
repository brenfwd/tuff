use crate::common;

use common::{MockSessionServer, start_server};

const STEVE: &str = "069a79f444e94726a5befca90e38aaf5";

async fn run(server: &common::TestServer, line: &str) -> serde_json::Value {
    let lines = server.handle.run_command(line).await.unwrap();
    lines[0].to_value()
}

#[tuff_core::watchdog]
#[tokio::test]
async fn fetchprofile_resolves_by_name_and_id_with_actions() {
    let (mock, session_url) = MockSessionServer::start().await;
    mock.add_profile("Steve", STEVE);
    let server = start_server("127.0.0.1:0", &session_url).await;

    let by_name = run(&server, "fetchprofile name steve").await;
    assert_eq!(by_name["translate"], "commands.fetchprofile.name.success");
    let actions = by_name["with"][1]["extra"].to_string();
    assert!(actions.contains("copy_to_clipboard"), "{actions}");
    assert!(actions.contains("textures"), "{actions}");

    let id = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
    let by_id = run(&server, &format!("fetchprofile id {id}")).await;
    assert_eq!(by_id["translate"], "commands.fetchprofile.id.success");

    let missing = run(&server, "fetchprofile name nobody").await;
    assert_eq!(missing["translate"], "commands.fetchprofile.name.failure");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}
