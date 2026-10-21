//! The optional audit log: what it records through region, session and
//! packet hooks, `/tuff audit` lookups, and rollback and restore of block
//! changes on both sides of a region border.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::protocol::play::Serverbound;
use crate::world::messages::{BlockOp, BlockOpResult, BreakMode};
use crate::world::session::PlayerSession;
use crate::world::session::hooks::{PacketInterceptor, SessionHooks};
use tuff_feature_builtin_audit_log::filter::Filter;
use tuff_feature_builtin_audit_log::record::Kind;
use tuff_feature_builtin_audit_log::store::Order;
use tuff_feature_builtin_audit_log::{AuditLog, AuditLogFeature};
use tuff_nbt::{Compound, Tag};
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::{Block, BlockState, Item};

const Y: i32 = 200;
/// Either side of the border between regions (-1, 0) and (0, 0).
const WEST: BlockPos = BlockPos::new(-1, Y, 3);
const EAST: BlockPos = BlockPos::new(0, Y, 3);
const CHEST: BlockPos = BlockPos::new(2, Y, 3);

fn start(dir: &Path) -> TestWorld {
    TestWorld::start_with_features(
        dir,
        vec![Box::new(AuditLogFeature::at(dir.join("audit.sqlite")))],
    )
}

fn log(world: &TestWorld) -> &AuditLog {
    world
        .shared
        .services
        .get::<AuditLog>()
        .expect("the service runs")
}

async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(|line| line.plain())
        .collect()
}

async fn blocks(region: &RegionHandle, op: BlockOp) -> BlockOpResult {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Blocks { op, reply }).await;
    answer.await.unwrap()
}

async fn paste(region: &RegionHandle, pos: BlockPos, state: &str, nbt: Option<Compound>) {
    let state: BlockState = state.parse().unwrap();
    let changes = vec![(pos, state, nbt)];
    let op = BlockOp::Paste {
        changes,
        strict: false,
    };
    blocks(region, op).await;
}

async fn copy(region: &RegionHandle, pos: BlockPos) -> (BlockState, Option<Compound>) {
    let positions = vec![pos];
    match blocks(region, BlockOp::Copy { positions }).await {
        BlockOpResult::Copied(mut found) => found.remove(0).unwrap(),
        other => panic!("{other:?}"),
    }
}

fn diamonds(nbt: Option<&Compound>) -> Option<i32> {
    let Some(Tag::List(slots)) = nbt?.get("Items") else {
        return None;
    };
    slots.first()?.as_compound()?.get_int("count")
}

async fn break_block(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    pos: BlockPos,
    sequence: i32,
) {
    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos,
            tool: None,
            mode: BreakMode::Finish,
            sequence,
        })
        .await;
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence: s, .. } if s == sequence => Some(()),
        _ => None,
    })
    .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaks_roll_back_and_restore_across_a_region_border() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let (alex, mut events) = PlayerHandle::new(100);
    world.publish_players(
        "minecraft:overworld",
        &[(&alex, [0.5, Y as f64 + 1.0, 3.5])],
    );
    let west = world.region(ChunkPos::of_block(WEST).region()).await;
    let east = world.region(ChunkPos::of_block(EAST).region()).await;
    assert_ne!(
        ChunkPos::of_block(WEST).region(),
        ChunkPos::of_block(EAST).region()
    );
    subscribe(&west, &alex, &mut events, &[ChunkPos::of_block(WEST)]).await;
    subscribe(&east, &alex, &mut events, &[ChunkPos::of_block(EAST)]).await;
    paste(&west, WEST, "minecraft:oak_log[axis=x]", None).await;
    paste(&east, EAST, "minecraft:stone", None).await;
    let chest = Compound::new().with("id", "minecraft:chest").with(
        "Items",
        Tag::List(vec![Tag::Compound(
            Compound::new()
                .with("Slot", Tag::Byte(0))
                .with("id", "minecraft:diamond")
                .with("count", Tag::Int(5)),
        )]),
    );
    paste(&east, CHEST, "minecraft:chest[facing=north]", Some(chest)).await;

    break_block(&west, &alex, &mut events, WEST, 1).await;
    break_block(&east, &alex, &mut events, EAST, 2).await;
    break_block(&east, &alex, &mut events, CHEST, 3).await;
    assert_eq!(copy(&east, CHEST).await.0.block(), Block::Air);

    let lookup = console(&world, "tuff audit lookup u:player100").await;
    assert!(lookup[0].contains("3 rows"), "{lookup:?}");
    assert!(
        lookup
            .iter()
            .any(|l| l.contains("player100 broke oak_log (-1 200 3)")),
        "{lookup:?}"
    );
    assert!(
        lookup.iter().any(|l| l.contains("broke chest")),
        "{lookup:?}"
    );
    assert_eq!(
        console(&world, "tuff audit lookup a:+block #count").await,
        ["0 rows"]
    );

    let rolled = console(&world, "tuff audit rollback t:1h r:#global").await;
    assert_eq!(rolled, ["Rolled back 3 blocks from 3 rows in 2 regions"]);
    assert_eq!(
        copy(&west, WEST).await.0,
        "minecraft:oak_log[axis=x]".parse::<BlockState>().unwrap()
    );
    assert_eq!(copy(&east, EAST).await.0.block(), Block::Stone);
    let (state, nbt) = copy(&east, CHEST).await;
    assert_eq!(state.block(), Block::Chest);
    assert_eq!(diamonds(nbt.as_ref()), Some(5));
    let lookup = console(&world, "tuff audit lookup").await;
    assert!(lookup[1].starts_with("[undone]"), "{lookup:?}");
    assert_eq!(
        console(&world, "tuff audit rollback t:1h r:#global").await,
        ["Rolled back 0 blocks from 0 rows in 0 regions"],
        "rows already rolled back are not rolled back twice"
    );

    let restored = console(&world, "tuff audit restore t:1h r:#global i:stone,oak_log").await;
    assert_eq!(restored, ["Restored 2 blocks from 2 rows in 2 regions"]);
    assert_eq!(copy(&west, WEST).await.0.block(), Block::Air);
    assert_eq!(copy(&east, EAST).await.0.block(), Block::Air);
    assert_eq!(copy(&east, CHEST).await.0.block(), Block::Chest);

    assert_eq!(
        console(&world, "tuff audit undo").await,
        ["Rolled back 2 blocks from 2 rows in 2 regions"]
    );
    assert_eq!(copy(&west, WEST).await.0.block(), Block::OakLog);
    assert_eq!(copy(&east, EAST).await.0.block(), Block::Stone);
    let lookup = console(&world, "tuff audit lookup u:player100 t:1h").await;
    assert!(lookup.last().unwrap().starts_with("Page 1/1"), "{lookup:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lookup_lines_link_to_teleports_and_reload_answers() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let reload = console(&world, "tuff audit reload").await;
    assert_eq!(reload, ["Audit log settings re-read"]);
    let rows = crate::world::commands::console::run(world.shared.clone(), "tuff audit near").await;
    assert_eq!(
        rows.iter().map(|l| l.plain()).collect::<Vec<_>>(),
        ["No results"]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn placements_are_logged_and_rolled_back_to_air() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alex, mut events) = PlayerHandle::new(100);
    world.publish_players("minecraft:overworld", &[(&alex, [3.5, 80.0, 3.5])]);
    subscribe(&region, &alex, &mut events, &[pos]).await;
    let floor = BlockPos::new(3, Generator::new(1).height(3, 3), 3);
    region
        .request(RegionRequest::Place(place(&alex, floor, Item::Stone, 1)))
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult { sequence: 1, .. } => Some(()),
        _ => None,
    })
    .await;
    let placed = BlockPos::new(3, floor.y + 1, 3);
    assert_eq!(copy(&region, placed).await.0.block(), Block::Stone);

    log(&world).sync().await.unwrap();
    let rows = log(&world)
        .lookup(Filter::default(), Order::NewestFirst, 10, 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, Some(Kind::BlockPlace));
    assert_eq!(rows[0].user, "player100");
    assert_eq!(rows[0].pos, [placed.x, placed.y, placed.z]);

    let rolled = console(&world, "tuff audit rollback t:5m a:+block").await;
    assert_eq!(rolled, ["Rolled back 1 blocks from 1 rows in 1 regions"]);
    assert_eq!(copy(&region, placed).await.0.block(), Block::Air);
    world.shutdown().await;
}

fn session_on(world: &TestWorld) -> PlayerSession {
    session_with_outbound(world).0
}

fn session_with_outbound(
    world: &TestWorld,
) -> (PlayerSession, mpsc::UnboundedReceiver<bytes::Bytes>) {
    let (outbound, received) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(7);
    let (_packets, packets) = mpsc::channel(8);
    let mut session = PlayerSession::new(
        world.shared.clone(),
        7,
        GameProfile {
            id: 77,
            name: "Steve".to_owned(),
            properties: Vec::new(),
        },
        ClientInformation::default(),
        outbound,
        handle,
        events,
        packets,
        PermissionLevel::All,
    );
    session.pose.position = [10.5, 70.0, -4.5];
    (session, received)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_commands_and_sessions_are_logged_and_purged() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let mut session = session_on(&world);
    let sessions = tuff_feature_builtin_audit_log::hooks::Sessions;
    let chat = tuff_feature_builtin_audit_log::hooks::ChatLog;
    sessions.joined(&mut session).await;
    chat.intercept(&mut session, &Serverbound::Chat("hello there".into()))
        .await;
    chat.intercept(
        &mut session,
        &Serverbound::ChatCommand("time set day".into()),
    )
    .await;
    sessions.leaving(&mut session).await;

    let lookup = console(&world, "tuff audit lookup u:steve").await;
    assert_eq!(lookup.len(), 6, "{lookup:?}");
    assert!(
        lookup[1].contains("Steve logged out (10 70 -5)"),
        "{lookup:?}"
    );
    assert!(lookup[2].contains("Steve ran /time set day"), "{lookup:?}");
    assert!(lookup[3].contains("Steve said hello there"), "{lookup:?}");
    assert!(lookup[4].contains("Steve logged in"), "{lookup:?}");
    assert_eq!(
        console(&world, "tuff audit lookup a:session #count").await,
        ["2 rows"]
    );
    assert_eq!(
        console(&world, "tuff audit lookup a:-session r:#nether #count").await,
        ["0 rows"]
    );

    world
        .shared
        .config
        .set_text_for_run("tuff.features.audit-log.log.chat", "false")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while log(&world).enabled(Kind::Chat) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the switch is followed");
    chat.intercept(&mut session, &Serverbound::Chat("unseen".into()))
        .await;
    assert_eq!(
        console(&world, "tuff audit lookup a:chat #count").await,
        ["1 rows"]
    );

    assert_eq!(
        console(&world, "tuff audit purge t:1h").await,
        ["Purged 0 rows"]
    );
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(
        console(&world, "tuff audit purge t:0s").await,
        ["Purged 4 rows"]
    );
    assert_eq!(console(&world, "tuff audit lookup").await, ["No results"]);
    log(&world).close().await;
    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("audit.sqlite-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_parameters_and_missing_time_are_refused() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let rollback = console(&world, "tuff audit rollback r:5").await;
    assert!(rollback[0].contains("needs t:"), "{rollback:?}");
    let lookup = console(&world, "tuff audit lookup a:nothing").await;
    assert!(lookup[0].contains("unknown action"), "{lookup:?}");
    let purge = console(&world, "tuff audit purge r:#world").await;
    assert!(purge[0].contains("needs one age"), "{purge:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_feature_nothing_is_logged_or_created() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    assert!(world.shared.services.get::<AuditLog>().is_none());
    let lookup = console(&world, "tuff audit lookup").await;
    assert!(!lookup.iter().any(|l| l.contains("rows")), "{lookup:?}");
    world.shutdown().await;
    let databases: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.contains("audit"))
        .collect();
    assert!(databases.is_empty(), "{databases:?}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_inspector_swallows_breaks_and_uses_only_while_on() {
    use crate::protocol::play::PlayerActionStatus;
    use crate::world::session::hooks::Verdict;
    let dir = TestDir::new();
    let world = start(dir.path());
    let mut session = session_on(&world);
    let inspector = tuff_feature_builtin_audit_log::hooks::Inspector;
    let dig = Serverbound::PlayerAction {
        status: PlayerActionStatus::StartDestroyBlock,
        position: BlockPos::new(1, 2, 3),
        face: 1,
        sequence: 4,
    };
    let other = Serverbound::Punch;
    assert_eq!(
        inspector.intercept(&mut session, &dig).await,
        Verdict::Allow
    );
    assert!(log(&world).toggle_inspector(7));
    assert_eq!(
        inspector.intercept(&mut session, &dig).await,
        Verdict::Cancel
    );
    assert_eq!(
        inspector.intercept(&mut session, &other).await,
        Verdict::Allow
    );
    assert!(!log(&world).toggle_inspector(7));
    assert_eq!(
        inspector.intercept(&mut session, &dig).await,
        Verdict::Allow
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn using_a_container_while_inspecting_shows_its_transactions() {
    use crate::world::session::hooks::Verdict;
    use tuff_feature_builtin_audit_log::record::{Record, Who};
    let dir = TestDir::new();
    let world = start(dir.path());
    let (mut session, mut received) = session_with_outbound(&world);
    let chest = BlockPos::new(1, 2, 3);
    let mut record = Record::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
        Who::cause("#test"),
        Kind::ContainerRemove,
        session.dimension,
        [chest.x, chest.y, chest.z],
    );
    record.material = Some("minecraft:zzdiamond".to_owned());
    record.amount = 3;
    log(&world).log(record);
    assert!(log(&world).toggle_inspector(7));
    let verdict = tuff_feature_builtin_audit_log::hooks::Inspector
        .intercept(
            &mut session,
            &Serverbound::UseItemOn {
                hand: 0,
                position: chest,
                face: tuff_world_model::block_pos::Face::Up,
                cursor: [0.5; 3],
                sequence: 1,
            },
        )
        .await;
    assert_eq!(verdict, Verdict::Cancel);
    let mut shown = Vec::new();
    while let Ok(bytes) = received.try_recv() {
        shown.extend_from_slice(&bytes);
    }
    assert!(
        shown.windows(10).any(|w| w == b"History of"),
        "the container history is listed"
    );
    world.shutdown().await;
}
