//! Game rules that gate portals, fire damage, command forks and pearls.

use super::*;
use crate::world::game_rules::RuleValue;
use crate::world::session::PlayerSession;
use tuff_feature_builtin_vanilla_dimensions::world::portal::{PortalContact, PortalKind};
use tuff_feature_builtin_vanilla_dimensions::world::session::portals::PortalExt;
use tuff_vanilla_data::damage_type;

fn session(world: &TestWorld, id: i32) -> PlayerSession {
    let (session, outbound) = session_with_outbound(world, id);
    std::mem::forget(outbound);
    session
}

fn set(world: &TestWorld, name: &str, value: RuleValue) {
    world.game_rules.send_modify(|rules| {
        rules.set(name, value).expect("a rule of that type");
    });
}

fn nether_contact() -> PortalContact {
    PortalContact {
        kind: PortalKind::Nether,
        pos: BlockPos::new(0, 64, 0),
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_fire_damage_rule_spares_burning_players() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    player.hurt_by(3.0, &damage_type::IN_FIRE, None, None).await;
    assert_eq!(player.health(), 17.0);
    set(&world, "fire_damage", RuleValue::Bool(false));
    player.hurt_by(3.0, &damage_type::ON_FIRE, None, None).await;
    assert_eq!(player.health(), 17.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nether_portal_delays_follow_the_rules() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set(
        &world,
        "players_nether_portal_default_delay",
        RuleValue::Int(3),
    );
    let mut player = session(&world, 1000);
    let mut ticks = 0;
    loop {
        player.inside_portal(nether_contact());
        ticks += 1;
        if player.tick_portal().is_some() {
            break;
        }
        assert!(ticks < 20, "the portal never fired");
    }
    assert_eq!(ticks, 4);

    set(
        &world,
        "allow_entering_nether_using_portals",
        RuleValue::Bool(false),
    );
    let mut other = session(&world, 1001);
    for _ in 0..10 {
        other.inside_portal(nether_contact());
        assert!(other.tick_portal().is_none());
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_fork_limit_stops_a_fanning_out_execute() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let run = |line: &'static str| {
        let shared = world.shared.clone();
        async move {
            crate::world::commands::console::run(shared, line)
                .await
                .iter()
                .map(|line| line.plain())
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        run("execute as @e run say hi").await,
        ["No entity was found"]
    );
    set(&world, "max_command_forks", RuleValue::Int(0));
    let out = run("execute positioned 0 0 0 run say hi").await;
    assert_eq!(out, ["Maximum number of contexts (0) reached"]);
    world.shutdown().await;
}
