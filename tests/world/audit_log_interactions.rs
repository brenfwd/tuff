//! The audit log's click, sign and username rows.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::world::messages::{BlockOp, BlockOpResult};
use crate::world::session::PlayerSession;
use crate::world::session::hooks::SessionHooks;
use tuff_feature_builtin_audit_log::filter::Filter;
use tuff_feature_builtin_audit_log::record::Kind;
use tuff_feature_builtin_audit_log::store::Order;
use tuff_feature_builtin_audit_log::{AuditLog, AuditLogFeature};
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::{Block, BlockState, Item};

fn start(dir: &Path) -> TestWorld {
    TestWorld::start_with_features(
        dir,
        vec![Box::new(AuditLogFeature::at(dir.join("audit.sqlite")))],
    )
}

async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(|line| line.plain())
        .collect()
}

async fn rows(
    world: &TestWorld,
    kinds: Vec<Kind>,
) -> Vec<tuff_feature_builtin_audit_log::store::Row> {
    let log = world.shared.services.get::<AuditLog>().unwrap();
    log.sync().await.unwrap();
    log.lookup(
        Filter {
            kinds,
            ..Filter::default()
        },
        Order::OldestFirst,
        100,
        0,
    )
    .await
    .unwrap()
}

async fn outcome(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

async fn paste(region: &RegionHandle, pos: BlockPos, state: BlockState) {
    let (reply, answer) = oneshot::channel();
    let op = BlockOp::Paste {
        changes: vec![(pos, state, None)],
        strict: false,
    };
    region.request(RegionRequest::Blocks { op, reply }).await;
    answer.await.unwrap();
}

async fn front_text(region: &RegionHandle, pos: BlockPos) -> Option<Tag> {
    let (reply, answer) = oneshot::channel();
    let op = BlockOp::Copy {
        positions: vec![pos],
    };
    region.request(RegionRequest::Blocks { op, reply }).await;
    match answer.await.unwrap() {
        BlockOpResult::Copied(mut found) => found
            .remove(0)?
            .1?
            .get_compound("front_text")?
            .get_list("messages")?
            .first()
            .cloned(),
        other => panic!("{other:?}"),
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clicking_a_lever_is_logged_but_a_stone_click_is_not() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let lever = BlockPos::new(3, 200, 3);
    let stone = BlockPos::new(5, 200, 3);
    paste(
        &region,
        lever,
        Block::Lever.default_state().with("face", "floor").unwrap(),
    )
    .await;
    paste(&region, stone, Block::Stone.default_state()).await;
    for (seq, pos) in [(1, stone), (2, lever)] {
        let mut click = place(&player, pos, Item::Stick, seq);
        click.item = None;
        region.request(RegionRequest::Place(click)).await;
        outcome(&mut events, seq).await;
    }
    front_text(&region, lever).await;
    let logged = rows(&world, vec![Kind::Click]).await;
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].pos, [3, 200, 3]);
    assert!(logged[0].old.as_deref().unwrap().contains("powered=false"));
    assert!(logged[0].new.as_deref().unwrap().contains("powered=true"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_edits_are_logged_and_roll_back_and_restore() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let floor = BlockPos::new(5, Generator::new(1).height(5, 5), 5);
    region
        .request(RegionRequest::Place(place(
            &player,
            floor,
            Item::OakSign,
            1,
        )))
        .await;
    outcome(&mut events, 1).await;
    let sign = BlockPos::new(5, floor.y + 1, 5);
    let eye = [5.5, sign.y as f64 + 1.6, 7.5];
    for text in ["first", "second"] {
        region
            .request(RegionRequest::UpdateSign {
                player: player.clone(),
                pos: sign,
                front: true,
                lines: vec![text.to_owned(), String::new(), String::new(), String::new()],
                eye,
                filtered: None,
            })
            .await;
        world.ticks(2).await;
        if text == "first" {
            region
                .request(RegionRequest::Place(place(&player, floor, Item::Stick, 2)))
                .await;
            region
                .request(RegionRequest::Place(place(&player, sign, Item::OakSign, 3)))
                .await;
        }
    }
    let logged = rows(&world, vec![Kind::Sign]).await;
    assert!(!logged.is_empty(), "{logged:?}");
    let last = logged.last().unwrap();
    assert!(
        last.text.as_deref().unwrap().contains("-> second"),
        "{last:?}"
    );

    let line = "tuff audit rollback t:1h r:#global a:sign";
    let rolled = console(&world, line).await;
    assert!(rolled[0].contains("Rolled back"), "{rolled:?}");
    world.ticks(2).await;
    assert_ne!(front_text(&region, sign).await, Some(Tag::from("second")));
    let restored = console(&world, "tuff audit restore t:1h r:#global a:sign").await;
    assert!(restored[0].contains("Restored"), "{restored:?}");
    world.ticks(2).await;
    assert_eq!(front_text(&region, sign).await, Some(Tag::from("second")));
    world.shutdown().await;
}

fn session_named(world: &TestWorld, name: &str) -> PlayerSession {
    let (outbound, _outbound) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(7);
    let (_packets, packets) = mpsc::channel(8);
    PlayerSession::new(
        world.shared.clone(),
        7,
        GameProfile {
            id: 77,
            name: name.to_owned(),
            properties: Vec::new(),
        },
        ClientInformation::default(),
        outbound,
        handle,
        events,
        packets,
        PermissionLevel::All,
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_changed_name_is_logged_once() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let sessions = tuff_feature_builtin_audit_log::hooks::Sessions;
    sessions.joined(&mut session_named(&world, "Steve")).await;
    sessions.joined(&mut session_named(&world, "Steve")).await;
    assert!(rows(&world, vec![Kind::Username]).await.is_empty());
    sessions.joined(&mut session_named(&world, "Alex")).await;
    let logged = rows(&world, vec![Kind::Username]).await;
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].text.as_deref(), Some("Steve -> Alex"));
    world.shutdown().await;
}
