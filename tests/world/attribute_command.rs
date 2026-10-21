//! `/attribute` on a player and on a mob: reading, base edits, modifiers
//! and the game's errors.

use super::*;
use crate::world::messages::{
    AttributeAnswer, AttributeCommand, AttributeOp, Directive, DirectiveSource,
};
use crate::world::session::PlayerSession;
use tuff_item::text::TextComponent;
use tuff_vanilla_data::item::ModifierOperation;
use tuff_vanilla_data::registry::{Attribute, EntityType};

async fn result(
    world: &TestWorld,
    player: &mut PlayerSession,
    holder: &str,
    command: &str,
) -> Option<i32> {
    player
        .run_command(&format!(
            "execute store result score {holder} out run {command}"
        ))
        .await;
    let shared = world.shared.clone();
    let holder = holder.to_owned();
    shared
        .scoreboard
        .read(move |board| board.score(&holder, "out").map(|s| s.value))
        .await
        .flatten()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attribute_reads_and_edits_players_and_mobs() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(18, 29)).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [20.5, SKY as f64 + 1.0, 20.5],
    )
    .await;
    world.ticks(2).await;
    let (mut player, _frames) = session_with_outbound(&world, 2000);
    player.pose.position = [24.5, SKY as f64 + 1.0, 24.5];
    player
        .run_command("scoreboard objectives add out dummy")
        .await;

    let cmd = |tail: &str| format!("attribute @e[type=pig,limit=1] minecraft:max_health {tail}");
    let health = 10;
    assert_eq!(
        result(&world, &mut player, "#a", &cmd("get")).await,
        Some(health)
    );
    assert_eq!(
        result(&world, &mut player, "#b", &cmd("get 10")).await,
        Some(health * 10)
    );
    assert_eq!(
        result(&world, &mut player, "#c", &cmd("base set 40")).await,
        Some(1)
    );
    assert_eq!(
        result(&world, &mut player, "#d", &cmd("base get")).await,
        Some(40)
    );
    assert_eq!(
        result(
            &world,
            &mut player,
            "#e",
            &cmd("modifier add test:m 0.5 add_multiplied_base")
        )
        .await,
        Some(1)
    );
    assert_eq!(
        result(
            &world,
            &mut player,
            "#f",
            &cmd("modifier add test:m 1 add_value")
        )
        .await,
        Some(0),
        "duplicate id"
    );
    assert_eq!(
        result(&world, &mut player, "#g", &cmd("get")).await,
        Some(60)
    );
    assert_eq!(
        result(
            &world,
            &mut player,
            "#h",
            &cmd("modifier value get test:m 100")
        )
        .await,
        Some(50)
    );
    assert_eq!(
        result(&world, &mut player, "#i", &cmd("modifier remove test:m")).await,
        Some(1)
    );
    assert_eq!(
        result(&world, &mut player, "#j", &cmd("modifier remove test:m")).await,
        Some(0),
        "missing modifier"
    );
    assert_eq!(
        result(&world, &mut player, "#k", &cmd("base reset")).await,
        Some(1)
    );
    assert_eq!(
        result(&world, &mut player, "#l", &cmd("get")).await,
        Some(health)
    );

    let source = || DirectiveSource {
        name: TextComponent::new("test"),
        player: None,
    };
    let ask = |attribute, op| Directive::Attribute(AttributeCommand { attribute, op });
    let speed = Attribute::MovementSpeed;
    let before = player.attribute_value(speed);
    let reply = player
        .apply_directive(ask(speed, AttributeOp::SetBase(0.5)), source())
        .await;
    assert_eq!(reply.attribute, Some(AttributeAnswer::Done));
    assert_eq!(player.attribute_value(speed), 0.5);
    let reply = player
        .apply_directive(
            ask(
                speed,
                AttributeOp::AddModifier {
                    id: "test:m".into(),
                    amount: 1.0,
                    operation: ModifierOperation::AddMultipliedTotal,
                },
            ),
            source(),
        )
        .await;
    assert_eq!(reply.attribute, Some(AttributeAnswer::Done));
    assert_eq!(player.attribute_value(speed), 1.0);
    let again = player
        .apply_directive(
            ask(
                speed,
                AttributeOp::AddModifier {
                    id: "test:m".into(),
                    amount: 1.0,
                    operation: ModifierOperation::AddValue,
                },
            ),
            source(),
        )
        .await;
    assert_eq!(again.attribute, Some(AttributeAnswer::DuplicateModifier));
    let saved = player.state();
    assert_eq!(saved.attributes, player.body.attributes);
    let reply = player
        .apply_directive(
            ask(speed, AttributeOp::RemoveModifier("test:m".into())),
            source(),
        )
        .await;
    assert_eq!(reply.attribute, Some(AttributeAnswer::Done));
    let reply = player
        .apply_directive(ask(speed, AttributeOp::ResetBase), source())
        .await;
    assert_eq!(reply.attribute, Some(AttributeAnswer::Number(before)));
    let reply = player
        .apply_directive(ask(Attribute::FollowRange, AttributeOp::Value), source())
        .await;
    assert_eq!(reply.attribute, Some(AttributeAnswer::NoAttribute));
    assert_eq!(
        result(
            &world,
            &mut player,
            "#m",
            "attribute @e[type=pig,limit=1] minecraft:attack_knockback get"
        )
        .await,
        Some(0),
        "a pig has no such attribute"
    );
    assert_eq!(mobs(&region, EntityType::Pig).await.len(), 1);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn player_max_health_follows_attribute() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _frames) = session_with_outbound(&world, 2000);
    player.body.attributes.set_base(Attribute::MaxHealth, 40.0);
    assert_eq!(player.max_health(), 40.0);
    player.body.health.value = 20.0;
    player.body.health.heal(30.0, player.max_health());
    assert_eq!(player.body.health.value, 40.0);
    player.body.attributes.set_base(Attribute::MaxHealth, 6.0);
    player.on_tick().await;
    assert_eq!(player.body.health.value, 6.0);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mob_attributes_reach_viewers_on_spawn_and_on_edit() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(18, 29)).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [20.5, SKY as f64 + 1.0, 20.5],
    )
    .await;
    world.ticks(2).await;
    let collect =
        |events: &mut tokio::sync::mpsc::UnboundedReceiver<crate::world::messages::PlayerEvent>,
         frames: &mut Vec<bytes::Bytes>| {
            while let Ok(event) = events.try_recv() {
                match event {
                    crate::world::messages::PlayerEvent::Track(track) => {
                        frames.extend(track.spawn.iter().cloned());
                    }
                    crate::world::messages::PlayerEvent::EntityPackets { packets, .. } => {
                        frames.extend(packets.iter().cloned());
                    }
                    _ => {}
                }
            }
        };
    let mut frames = Vec::new();
    collect(&mut events, &mut frames);
    let spawned = synced_attribute_values(&frames);
    assert_eq!(spawned.get(&Attribute::MaxHealth), Some(&10.0));
    assert!(!spawned.contains_key(&Attribute::FollowRange));

    let (mut player, _frames) = session_with_outbound(&world, 2000);
    player.pose.position = [24.5, SKY as f64 + 1.0, 24.5];
    player
        .run_command("attribute @e[type=pig,limit=1] minecraft:scale base set 3")
        .await;
    world.ticks(2).await;
    frames.clear();
    collect(&mut events, &mut frames);
    let edited = synced_attribute_values(&frames);
    assert_eq!(edited.get(&Attribute::Scale), Some(&3.0));
}
