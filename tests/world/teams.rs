//! Teams in play: friendly fire between players, mobs leaving their
//! teammates alone, and collision rules keeping mobs from pushing.

use super::*;
use crate::world::entity::Culprit;
use crate::world::game_rules::RuleValue;
use crate::world::messages::PlayerLooks;
use crate::world::scoreboard::{CollisionRule, Team};
use crate::world::session::DamageSource;
use tuff_storage::admin::format_uuid;
use tuff_vanilla_data::registry::EntityType;

async fn team(
    world: &TestWorld,
    members: Vec<String>,
    change: impl FnOnce(&mut Team) + Send + 'static,
) {
    world
        .shared
        .scoreboard
        .update(move |board| {
            let mut team = Team::new("red");
            change(&mut team);
            board.add_team(team);
            for member in members {
                board.join_team(&member, "red");
            }
        })
        .await
        .unwrap();
}

fn hit_by(kind: EntityType, name: &str) -> DamageSource {
    DamageSource {
        kind: &tuff_vanilla_data::damage_type::PLAYER_ATTACK,
        attacker: Some(match kind {
            EntityType::Player => Culprit::player(9, 9, name),
            _ => Culprit {
                id: 9,
                uuid: 9,
                kind,
                name: TextComponent::new(name),
                charged: None,
                owner_name: None,
                held_name: None,
            },
        }),
        projectile: None,
        position: None,
        weapon: None,
        direct: true,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teammates_without_friendly_fire_cannot_hurt_each_other() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    team(&world, vec!["Player1".into(), "Mate".into()], |team| {
        team.friendly_fire = false
    })
    .await;
    let before = session.body.health.value;
    let spared = session.hurt(hit_by(EntityType::Player, "Mate"), 2.0).await;
    assert!(!spared.landed);
    assert_eq!(session.body.health.value, before);
    assert!(
        session
            .hurt(hit_by(EntityType::Player, "Stranger"), 2.0)
            .await
            .landed
    );

    world
        .shared
        .scoreboard
        .update(|board| board.modify_team("red", |team| team.friendly_fire = true))
        .await;
    assert!(
        session
            .hurt(hit_by(EntityType::Player, "Mate"), 5.0)
            .await
            .landed
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pvp_rule_spares_players_from_players_only() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    assert!(world.shared.rule_bool("pvp"), "on by default");
    world.game_rules.send_modify(|rules| {
        rules.set("pvp", RuleValue::Bool(false)).unwrap();
    });
    let (mut session, _out) = session_with_outbound(&world, 1);
    assert!(
        !session
            .hurt(hit_by(EntityType::Player, "Stranger"), 2.0)
            .await
            .landed
    );
    assert!(
        session
            .hurt(hit_by(EntityType::Zombie, "Zombie"), 2.0)
            .await
            .landed
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombies_leave_their_teammates_alone() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let zombie = mobs(&region, EntityType::Zombie).await[0].uuid;
    team(&world, vec!["Tester".into(), format_uuid(zombie)], |_| {}).await;
    let position = [6.5, SKY as f64 + 1.0, 6.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    for _ in 0..200 {
        world.ticks(1).await;
        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(event, PlayerEvent::EntityHurt(_)),
                "a teammate attacked"
            );
        }
    }
    world
        .shared
        .scoreboard
        .update(|board| board.leave_team("Tester"))
        .await;
    let hit = next_hit(&world, &mut events, 200).await;
    assert_eq!(hit.attacker.unwrap().kind, EntityType::Zombie);
    world.shutdown().await;
}

async fn spread_after_overlap(rule: Option<CollisionRule>) -> f64 {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    for x in [7.4, 7.6] {
        summon(
            &region,
            EntityType::Pig,
            Compound::new().with("NoAI", true),
            [x, y, 7.5],
        )
        .await;
    }
    if let Some(rule) = rule {
        let pigs = mobs(&region, EntityType::Pig).await;
        let members = pigs.iter().map(|pig| format_uuid(pig.uuid)).collect();
        team(&world, members, move |team| team.collision_rule = rule).await;
    }
    world.ticks(10).await;
    let pigs = mobs(&region, EntityType::Pig).await;
    let spread = (pigs[0].position[0] - pigs[1].position[0]).abs();
    world.shutdown().await;
    spread
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collision_rules_decide_whether_mobs_push_apart() {
    assert!(spread_after_overlap(None).await > 0.4);
    assert!(spread_after_overlap(Some(CollisionRule::PushOtherTeams)).await > 0.4);
    assert!(spread_after_overlap(Some(CollisionRule::Never)).await < 0.25);
    assert!(spread_after_overlap(Some(CollisionRule::PushOwnTeam)).await < 0.25);
}
