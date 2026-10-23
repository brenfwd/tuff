//! The world border in play: damage past its safe zone, moves that push
//! through it from inside, interaction outside it, its packets, and the
//! `/worldborder` command against the live state.

use super::*;
use crate::protocol::play::Movement;
use crate::world::border::Border;
use crate::world::session::PlayerSession;
use tuff_vanilla_data::packet::play::Clientbound;
use tuff_vanilla_data::registry::EntityType;

const OVERWORLD_KEY: &str = "minecraft:overworld";

fn set_border(world: &TestWorld, border: Border) {
    let id = world.dimension(OVERWORLD_KEY);
    world.borders[id.index()].send_replace(border);
}

fn narrow(damage_per_block: f64, safe_zone: f64) -> Border {
    let mut border = Border::fixed([0.0, 0.0], 20.0);
    border.damage_per_block = damage_per_block;
    border.safe_zone = safe_zone;
    border
}

fn session(world: &TestWorld, id: i32) -> (PlayerSession, mpsc::UnboundedReceiver<bytes::Bytes>) {
    let (mut session, outbound) = session_with_outbound(world, id);
    session.pose.position = [0.5, 80.0, 0.5];
    (session, outbound)
}

fn walk(to: [f64; 3]) -> Movement {
    Movement {
        position: Some(to),
        rotation: None,
        on_ground: false,
        horizontal_collision: false,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_past_the_safe_zone_take_damage_per_block() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set_border(&world, narrow(1.0, 2.0));
    let (mut player, _outbound) = session(&world, 1000);

    player.pose.position = [11.5, 80.0, 0.5];
    assert_eq!(player.border_damage(), None, "within the safe zone");
    player.pose.position = [9.8, 80.0, 0.5];
    assert_eq!(
        player.border_damage(),
        None,
        "inside, though its box pokes out"
    );
    player.pose.position = [15.5, 80.0, 0.5];
    assert_eq!(player.border_damage(), Some(3.0));
    player.pose.position = [12.1, 80.0, 0.5];
    assert_eq!(player.border_damage(), Some(1.0), "at least one point");

    player.pose.position = [15.5, 80.0, 0.5];
    player.run_tick().await;
    assert_eq!(player.health(), 17.0);

    set_border(&world, narrow(0.0, 2.0));
    assert_eq!(player.border_damage(), None, "no damage per block");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moves_from_inside_stop_at_the_border_and_moves_outside_are_free() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set_border(&world, narrow(0.2, 5.0));

    let (mut player, _outbound) = session(&world, 1000);
    player.pose.position = [9.0, 80.0, 0.5];
    player.handle_move(walk([9.7, 80.0, 0.5])).await;
    assert_eq!(player.pose.position, [9.7, 80.0, 0.5], "up to the edge");
    player.handle_move(walk([10.5, 80.0, 0.5])).await;
    assert_eq!(
        player.pose.position,
        [9.7, 80.0, 0.5],
        "through it is refused"
    );

    let (mut outside, _outbound) = session(&world, 1001);
    outside.pose.position = [30.0, 80.0, 0.5];
    outside.handle_move(walk([29.0, 80.0, 0.5])).await;
    assert_eq!(outside.pose.position, [29.0, 80.0, 0.5]);
    outside.handle_move(walk([31.0, 80.0, 0.5])).await;
    assert_eq!(outside.pose.position, [31.0, 80.0, 0.5]);

    let (mut spectator, _outbound) = session(&world, 1002);
    spectator.mode.game_mode = protocol::play::GameMode::Spectator;
    spectator.pose.position = [9.0, 80.0, 0.5];
    spectator.handle_move(walk([11.0, 80.0, 0.5])).await;
    assert_eq!(
        spectator.pose.position,
        [11.0, 80.0, 0.5],
        "spectators pass"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocks_outside_the_border_cannot_be_broken() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set_border(&world, narrow(0.2, 5.0));
    let (mut player, _outbound) = session(&world, 1000);
    player.pose.position = [8.5, 80.0, 0.5];
    assert!(player.may_destroy(BlockPos::new(9, 80, 0)));
    assert!(!player.may_destroy(BlockPos::new(10, 80, 0)));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn border_changes_reach_only_players_in_that_dimension() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = session(&world, 1000);
    while outbound.try_recv().is_ok() {}
    let packet = crate::world::framed(&protocol::play::SetBorderSize(64.0));
    let nether = world.dimension("minecraft:the_nether");
    player
        .handle_event(PlayerEvent::Border {
            dimension: nether,
            packet: packet.clone(),
        })
        .await;
    assert!(outbound.try_recv().is_err());
    player
        .handle_event(PlayerEvent::Border {
            dimension: player.dimension_id(),
            packet,
        })
        .await;
    let sent = outbound.try_recv().expect("the change is sent");
    assert_eq!(framed_packet_id(&sent), Clientbound::SetBorderSize.id());
    let mut payload = framed_payload(&sent);
    payload.read_var_int().unwrap();
    assert_eq!(payload.read_double().unwrap(), 64.0);

    let mut border = Border::fixed([4.0, -4.0], 100.0);
    border
        .apply(crate::world::border::BorderChange::Set {
            size: 50.0,
            ticks: 200,
        })
        .unwrap();
    let init = crate::world::framed(&border.initialize());
    assert_eq!(framed_packet_id(&init), Clientbound::InitializeBorder.id());
    let mut payload = framed_payload(&init);
    payload.read_var_int().unwrap();
    let doubles: Vec<f64> = (0..4).map(|_| payload.read_double().unwrap()).collect();
    assert_eq!(doubles, [4.0, -4.0, 100.0, 50.0]);
    assert_eq!(payload.read_var_long().unwrap(), 200);
    assert_eq!(payload.read_var_int().unwrap(), 29_999_984);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_command_changes_the_live_border_and_ticks_move_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session(&world, 1000);
    let overworld = world.dimension(OVERWORLD_KEY);
    let border = || world.shared.dimension(overworld).border();

    player.run_command("worldborder set 100").await;
    player.run_command("worldborder center 10 -20").await;
    assert_eq!(border().size(), 100.0);
    assert_eq!(border().center, [10.5, -19.5]);
    player.run_command("worldborder set 50 10").await;
    assert_eq!((border().size(), border().lerp_time()), (100.0, 10));
    world.ticks(4).await;
    assert_eq!((border().size(), border().lerp_time()), (80.0, 6));
    player.run_command("worldborder add 10 2").await;
    assert_eq!((border().lerp_target(), border().lerp_time()), (90.0, 8));
    world.ticks(8).await;
    assert!(!border().is_moving());
    assert_eq!(border().size(), 90.0);

    player.run_command("worldborder damage amount 1.5").await;
    player.run_command("worldborder damage buffer 2").await;
    player.run_command("worldborder warning distance 9").await;
    player.run_command("worldborder warning time 3s").await;
    let now = border();
    assert_eq!(
        (
            now.damage_per_block,
            now.safe_zone,
            now.warning_blocks,
            now.warning_time
        ),
        (1.5, 2.0, 9, 60)
    );

    player
        .run_command("scoreboard objectives add out dummy")
        .await;
    player
        .run_command("execute store result score #get out run worldborder get")
        .await;
    player
        .run_command("execute store result score #grow out run worldborder add 20")
        .await;
    player
        .run_command("execute store success score #same out run worldborder set 110")
        .await;
    let score = |holder: &'static str| {
        let shared = world.shared.clone();
        async move {
            shared
                .scoreboard
                .read(move |board| board.score(holder, "out").map(|s| s.value))
                .await
                .flatten()
        }
    };
    assert_eq!(score("#get").await, Some(90));
    assert_eq!(score("#grow").await, Some(20));
    assert_eq!(score("#same").await, Some(0));
    assert_eq!(border().size(), 110.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn items_inside_the_border_stop_at_its_edge_but_not_without_one() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set_border(&world, narrow(0.2, 5.0));
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let motion = Compound::new()
        .with(
            "Item",
            tuff_item::item::ItemPicture::new(Item::Stone, 1).to_nbt(),
        )
        .with(
            "Motion",
            Tag::List(vec![Tag::Double(0.4), Tag::Double(0.0), Tag::Double(0.0)]),
        );
    summon(&region, EntityType::Item, motion, [8.0, 80.0, 0.5]).await;
    world.ticks(40).await;
    let items = mobs(&region, EntityType::Item).await;
    assert_eq!(items.len(), 1);
    assert!(items[0].position[0] <= 10.0, "{:?}", items[0].position);
    world.shutdown().await;
}
