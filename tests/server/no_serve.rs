use crate::common;

use common::TempDir;
use tuff::scripts::run_scripts;
use tuff_core::Server;
use tuff_core::ServerHandle;

struct Quiet {
    handle: ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    dir: TempDir,
}

async fn start_quiet() -> Quiet {
    let dir = TempDir::new();
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(dir.path().to_path_buf())
        .serve(false)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    Quiet { handle, task, dir }
}

fn script(quiet: &Quiet, name: &str, text: &str) -> String {
    let path = quiet.dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path.to_string_lossy().into_owned()
}

#[tuff_core::watchdog]
#[tokio::test]
async fn no_serve_binds_no_listeners_but_runs_commands() {
    let quiet = start_quiet().await;
    assert!(quiet.handle.local_addrs().await.unwrap().is_empty());
    assert!(quiet.handle.http_addrs().await.unwrap().is_empty());
    let feedback = quiet
        .handle
        .run_command("gamerule random_tick_speed")
        .await
        .unwrap();
    assert!(!feedback.is_empty());
    quiet.handle.shutdown().await.unwrap();
    quiet.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn execute_runs_files_in_order_and_reports_failures() {
    let quiet = start_quiet().await;
    let first = script(&quiet, "a.txt", "# set\ngamerule random_tick_speed 5\n");
    let second = script(
        &quiet,
        "b.txt",
        "/gamerule random_tick_speed 7\nnot_a_command\n",
    );
    let missing = quiet
        .dir
        .path()
        .join("missing.txt")
        .to_string_lossy()
        .into_owned();
    let reports = run_scripts(&quiet.handle, &[first, second, missing])
        .await
        .unwrap();
    assert_eq!(reports.len(), 3);
    assert!(reports[0].ok, "{:?}", reports[0]);
    assert!(!reports[1].ok);
    assert!(
        reports[1]
            .feedback
            .iter()
            .any(|line| line.contains("Line 2")),
        "{:?}",
        reports[1]
    );
    assert!(!reports[2].ok);
    let value = quiet
        .handle
        .run_command("gamerule random_tick_speed")
        .await
        .unwrap();
    assert!(
        value
            .iter()
            .any(|line| line.with.iter().any(|arg| arg.text == "7")),
        "{value:?}"
    );
    quiet.handle.shutdown().await.unwrap();
    quiet.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn cli_execute_allows_absolute_paths_but_command_does_not() {
    let quiet = start_quiet().await;
    let path = script(&quiet, "abs.txt", "gamerule random_tick_speed 4\n");
    let reports = run_scripts(&quiet.handle, std::slice::from_ref(&path))
        .await
        .unwrap();
    assert!(reports[0].ok, "{:?}", reports[0]);
    let feedback = quiet
        .handle
        .run_command(format!("tuff execute \"{path}\" as console"))
        .await
        .unwrap();
    assert!(
        feedback
            .iter()
            .any(|l| l.plain().contains("outside the working directory")),
        "{feedback:?}"
    );
    quiet.handle.shutdown().await.unwrap();
    quiet.task.await.unwrap().unwrap();
}
