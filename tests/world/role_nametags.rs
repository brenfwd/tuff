//! Role nametags reach viewers through the scoreboard actor: a viewer
//! subscribing later sees them, role changes refresh subscribed viewers,
//! and vanilla team membership keeps precedence.

use super::*;
use crate::world::scoreboard::{Nametag, TeamColor};
use bytes::Bytes;

fn tag(prefix: &str) -> Nametag {
    Nametag {
        prefix: tuff_item::text::TextComponent::new(prefix),
        suffix: tuff_item::text::TextComponent::default(),
        color: Some(TeamColor::Gold),
    }
}

fn board() -> crate::world::scoreboard::ScoreboardHandle {
    let shared = bare_shared();
    crate::world::scoreboard::start(shared.backend.clone(), shared.clock.clone()).0
}

async fn drain(viewer: &mut mpsc::UnboundedReceiver<Bytes>) -> Vec<Bytes> {
    let mut out = Vec::new();
    while let Ok(Some(packet)) =
        tokio::time::timeout(std::time::Duration::from_millis(200), viewer.recv()).await
    {
        out.push(packet);
    }
    out
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewers_see_role_nametags_when_joining_and_on_change() {
    let scoreboard = board();
    scoreboard.set_nametags(vec![("Alex".to_owned(), Some(tag("[A] ")))]);
    scoreboard.flush().await;

    let (early_tx, mut early) = mpsc::unbounded_channel();
    scoreboard.subscribe(early_tx).await;
    let joined = drain(&mut early).await;
    assert_eq!(joined.len(), 1);

    scoreboard.set_nametags(vec![("Alex".to_owned(), Some(tag("[B] ")))]);
    let refreshed = drain(&mut early).await;
    assert_eq!(refreshed.len(), 1);
    assert_ne!(refreshed[0], joined[0]);

    let (late_tx, mut late) = mpsc::unbounded_channel();
    scoreboard.subscribe(late_tx).await;
    assert_eq!(drain(&mut late).await, refreshed_as_join(&refreshed).await);

    scoreboard.set_nametags(vec![("Alex".to_owned(), None)]);
    assert_eq!(drain(&mut early).await.len(), 1);
    assert_eq!(drain(&mut late).await.len(), 1);
}

async fn refreshed_as_join(refreshed: &[Bytes]) -> Vec<Bytes> {
    let scoreboard = board();
    scoreboard.set_nametags(vec![("Alex".to_owned(), Some(tag("[B] ")))]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    scoreboard.subscribe(tx).await;
    let join = drain(&mut rx).await;
    assert_eq!(join.len(), refreshed.len());
    join
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vanilla_team_member_gets_no_role_packets() {
    let scoreboard = board();
    scoreboard
        .update(|b| {
            b.add_team(crate::world::scoreboard::Team::new("staff"));
            b.join_team("Alex", "staff");
        })
        .await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    scoreboard.subscribe(tx).await;
    let before = drain(&mut rx).await;
    scoreboard.set_nametags(vec![("Alex".to_owned(), Some(tag("[A] ")))]);
    scoreboard.flush().await;
    assert!(drain(&mut rx).await.is_empty());
    assert_eq!(before.len(), 1);
}
