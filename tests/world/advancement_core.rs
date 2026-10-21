//! The advancement framework lives in core: with the vanilla tree absent, a
//! plugin's own tree and trigger reach the client and persist.

use std::sync::Arc;

use super::*;
use crate::feature::{Feature, LoadContext};
use crate::world::advancement::trigger::{Event, Handler};
use crate::world::advancement::{Definition, SessionAdvancements};
use crate::world::messages::{Directive, DirectiveSource};
use tuff_item::load::LoadError;
use tuff_vanilla_data::advancement as data;
use tuff_vanilla_data::registry::EntityType;

struct Quests;

const CRITERIA: &[data::Criterion] = &[data::Criterion {
    name: "go",
    trigger: "quests:visit",
    conditions: None,
}];

fn definition(name: &'static str, parent: Option<&'static str>) -> Definition {
    Definition {
        name,
        parent,
        display: Some(data::Display {
            icon: data::Icon {
                item: Item::Paper,
                count: 1,
                components: None,
            },
            title: "\"Quest\"",
            description: "\"Do it\"",
            frame: data::Frame::Task,
            background: parent.is_none().then_some("minecraft:block/stone"),
            show_toast: true,
            announce_to_chat: false,
            hidden: false,
        }),
        criteria: CRITERIA,
        requirements: &[&["go"]],
        rewards: data::Rewards::NONE,
        sends_telemetry_event: false,
        position: [0.0, 0.0],
    }
}

impl Feature for Quests {
    fn id(&self) -> &'static str {
        "quests"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() != tuff_item::load::Phase::Content {
            return Ok(());
        }
        let id = cx.feature_id();
        let registries = cx.registries();
        let handler: Handler = Arc::new(|_, event, _| {
            matches!(
                event,
                Event::Custom {
                    trigger: "quests:visit",
                    ..
                }
            )
        });
        registries
            .advancement_triggers
            .register(id, "quests:visit", handler)?;
        registries.advancement_definitions.register(
            id,
            "quests:root",
            definition("quests:root", None),
        )?;
        registries.advancement_definitions.register(
            id,
            "quests:next",
            definition("quests:next", Some("quests:root")),
        )
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_tree_and_trigger_work_without_the_vanilla_tree() {
    let dir = TestDir::new();
    let world = TestWorld::start_without_survival(dir.path(), vec![Box::new(Quests)]);
    let (mut player, mut outbound) = session_with_outbound(&world, 5);
    player.stand_at([0.5, 100.0, 0.5], true);
    assert_eq!(player.progress.advancement_tree.len(), 2);
    assert!(
        player
            .progress
            .advancement_tree
            .id("minecraft:story/root")
            .is_none()
    );

    player.fire(Event::Custom {
        trigger: "quests:visit",
        data: &(),
    });
    player.run_tick().await;
    assert!(player.advancement_done("quests:root"));
    assert!(player.advancement_done("quests:next"));
    let id = tuff_vanilla_data::packet::play::Clientbound::UpdateAdvancements.id();
    let mut names = Vec::new();
    while let Ok(frame) = outbound.try_recv() {
        let mut packet = framed_payload(&frame);
        if packet.read_var_int().unwrap() != id {
            continue;
        }
        assert!(packet.read_bool().unwrap());
        assert_eq!(packet.read_var_int().unwrap(), 2);
        names.push(packet.read_string().unwrap());
    }
    assert_eq!(names, ["quests:root"]);

    player.save().await;
    let (mut again, _outbound) = session_with_outbound(&world, 5);
    again.load_saved().await;
    assert!(again.advancement_done("quests:root"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_advancement_directive_grants_and_revokes_a_plugin_tree_without_survival() {
    let dir = TestDir::new();
    let world = TestWorld::start_without_survival(dir.path(), vec![Box::new(Quests)]);
    let (mut player, _outbound) = session_with_outbound(&world, 6);
    player.stand_at([0.5, 100.0, 0.5], true);
    let tree = player.progress.advancement_tree.clone();
    let source = || DirectiveSource {
        name: TextComponent::new("Server"),
        player: None,
    };
    let directive = |grant| Directive::Advancement {
        grant,
        tree: tree.clone(),
        ids: vec![tree.id("quests:root").unwrap()],
        criterion: None,
        show: true,
    };
    assert_eq!(
        player
            .apply_directive(directive(true), source())
            .await
            .value,
        1
    );
    player.run_tick().await;
    assert!(player.advancement_done("quests:root"));
    assert_eq!(
        player
            .apply_directive(directive(false), source())
            .await
            .value,
        1
    );
    player.run_tick().await;
    assert!(!player.advancement_done("quests:root"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn summoning_an_experience_orb_without_survival_keeps_it_as_an_inert_entity() {
    let dir = TestDir::new();
    let world = TestWorld::start_without_survival(dir.path(), Vec::new());
    let region = world.region(RegionPos { x: 0, z: 0 }).await;
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new().with("id", "minecraft:experience_orb"),
            position: [8.5, 100.0, 8.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    world.ticks(2).await;
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let kinds: Vec<_> = answer.await.unwrap().into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds, [EntityType::ExperienceOrb]);
    world.shutdown().await;
}
