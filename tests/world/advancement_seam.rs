//! A session talks to advancements only through the owner it holds: an
//! owner that is not the built-in tree sees every event, grant, flush,
//! tab, reload and reward hand-out.

use std::any::Any;
use std::sync::Mutex;

use super::*;
use crate::world::advancement::trigger::Event;
use crate::world::advancement::{Advancements, Id, Overlay, ProgressView, Related, Reward, View};
use crate::world::messages::{Directive, DirectiveSource};
use crate::world::session::PlayerSession;
use crate::world::session::hooks::PlayerSaved;
use tuff_core::world::advancement::SavedProgress;

const NAMES: [&str; 2] = ["t:a", "t:b"];

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<String>>,
    rewards: Mutex<Vec<Reward>>,
}

impl Recorder {
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn calls(&self) -> Vec<String> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

impl Advancements for Recorder {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn fire(&self, _: &mut PlayerSession, event: Event<'_>) {
        match event {
            Event::Tick => self.record("tick".into()),
            Event::ItemDurabilityChanged { delta, .. } => {
                self.record(format!("durability {delta}"));
            }
            _ => {}
        }
    }

    fn view(self: Arc<Self>, _: &mut PlayerSession) -> View {
        View(self)
    }

    fn set(
        &self,
        _: &mut PlayerSession,
        grant: bool,
        ids: &[Id],
        criterion: Option<&str>,
        show: bool,
    ) -> i32 {
        self.record(format!("set {grant} {ids:?} {criterion:?} {show}"));
        ids.len() as i32
    }

    fn flush(&self, _: &mut PlayerSession, show: bool) {
        self.record(format!("flush {show}"));
    }

    fn take_rewards(&self, _: &mut PlayerSession) -> Vec<Reward> {
        std::mem::take(&mut *self.rewards.lock().unwrap())
    }

    fn seen_tab(&self, _: &mut PlayerSession, tab: &str) {
        self.record(format!("tab {tab}"));
    }

    fn rebind(&self, _: &mut PlayerSession, old: &dyn Advancements) {
        self.record(format!("rebind from {}", old.len()));
    }

    fn len(&self) -> usize {
        NAMES.len()
    }

    fn id(&self, name: &str) -> Option<Id> {
        NAMES.iter().position(|n| *n == name).map(|i| i as Id)
    }

    fn name(&self, id: Id) -> &str {
        NAMES[usize::from(id)]
    }

    fn has_criterion(&self, _: Id, criterion: &str) -> bool {
        criterion == "c"
    }

    fn chat_name(&self, id: Id) -> TextComponent {
        TextComponent::new(self.name(id))
    }

    fn related(&self, id: Id, _: Related) -> Vec<Id> {
        vec![id]
    }

    fn with_overlay(&self, _: Overlay<'_>) -> (Arc<dyn Advancements>, Vec<String>) {
        (Arc::new(Recorder::default()), Vec::new())
    }
}

impl ProgressView for Recorder {
    fn done(&self, name: &str) -> Option<bool> {
        self.id(name).map(|id| id == 1)
    }

    fn criterion(&self, _: &str, _: &str) -> Option<bool> {
        None
    }
}

fn source() -> DirectiveSource {
    DirectiveSource {
        name: TextComponent::new("Server"),
        player: None,
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_session_asks_only_the_owner_it_holds() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 1);
    let owner = Arc::new(Recorder::default());
    player.progress.advancement_tree = owner.clone();
    player.stand_at([0.5, 100.0, 0.5], true);

    player.run_tick().await;
    let calls = owner.calls();
    assert!(calls.contains(&"tick".to_owned()), "{calls:?}");
    assert_eq!(calls.last().map(String::as_str), Some("flush true"));

    let view = player.advancement_view();
    assert_eq!(view.done("t:b"), Some(true));
    assert_eq!(view.done("t:a"), Some(false));
    assert_eq!(view.done("minecraft:story/root"), None);

    let other: Arc<dyn Advancements> = Arc::new(Recorder::default());
    let reply = player
        .apply_directive(
            Directive::Advancement {
                grant: true,
                tree: other,
                ids: vec![1],
                criterion: Some("c".into()),
                show: true,
            },
            source(),
        )
        .await;
    assert_eq!(reply.value, 1);
    assert_eq!(owner.calls(), [r#"set true [1] Some("c") true"#]);

    player.seen_advancements(Some("t:a".into()));
    assert_eq!(owner.calls(), ["tab t:a"]);

    let before = player.experience().total;
    owner.rewards.lock().unwrap().push(Reward {
        experience: 7,
        ..Reward::default()
    });
    player.run_tick().await;
    assert_eq!(player.experience().total, before + 7);
    owner.calls();

    assert!(
        SavedProgress.save(&player).is_none(),
        "the built-in progress document belongs to the built-in tree"
    );

    let reloaded = Arc::new(Recorder::default());
    player.rebind_advancements(reloaded.clone());
    assert_eq!(reloaded.calls(), ["rebind from 2"]);
    assert!(owner.calls().is_empty());
    player.rebind_advancements(reloaded.clone());
    assert!(reloaded.calls().is_empty(), "the same owner is not rebound");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn wearing_an_item_reports_durability_including_the_breaking_hit() {
    use tuff_item::item::{ItemStack, MintReason};
    use tuff_vanilla_data::Item;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 1);
    let owner = Arc::new(Recorder::default());
    player.progress.advancement_tree = owner.clone();
    player.mode.abilities.instabuild = false;
    let mut helmet = ItemStack::mint(Item::LeatherHelmet, 1, MintReason::Test);
    helmet.set_damage(helmet.max_damage().unwrap() - 1);
    player.inventory.set_hand(false, Some(helmet));
    owner.calls();
    player.hurt_hand(false, 1).await;
    assert_eq!(owner.calls(), ["durability -1"]);
    assert!(player.inventory.hand(false).is_none());
    world.shutdown().await;
}
