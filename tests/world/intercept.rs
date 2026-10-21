use super::*;
use std::sync::Mutex;

use tuff_core::feature::{Feature, LoadContext};
use tuff_core::protocol::play::Serverbound;
use tuff_core::world::session::hooks::{Interceptor, PacketInterceptor, Verdict};
use tuff_item::load::{LoadError, Phase};

struct Probe {
    name: &'static str,
    seen: Arc<Mutex<Vec<(&'static str, Serverbound)>>>,
}

impl PacketInterceptor for Probe {
    fn intercept<'a>(
        &'a self,
        _session: &'a mut tuff_core::world::session::PlayerSession,
        packet: &'a Serverbound,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Verdict> + Send + 'a>> {
        self.seen.lock().unwrap().push((self.name, packet.clone()));
        let verdict = match (self.name, packet) {
            ("first", Serverbound::SetCarriedItem(1)) => {
                Verdict::Correct(Box::new(Serverbound::SetCarriedItem(2)))
            }
            ("first", Serverbound::SetCarriedItem(5)) => Verdict::Cancel,
            ("first", Serverbound::Chat(text)) if text == "kick" => {
                Verdict::Kick(tuff_item::text::TextComponent::new("no"))
            }
            _ => Verdict::Allow,
        };
        Box::pin(async move { verdict })
    }
}

struct Plug(Arc<Mutex<Vec<(&'static str, Serverbound)>>>);

impl Feature for Plug {
    fn id(&self) -> &'static str {
        "test-intercept"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            let registries = cx.registries();
            for (name, order) in [("second", 20), ("first", 10)] {
                registries.packet_interceptors.push(Interceptor {
                    order,
                    hook: Arc::new(Probe {
                        name,
                        seen: self.0.clone(),
                    }),
                });
            }
        }
        Ok(())
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interceptors_run_in_order_and_may_correct_cancel_or_kick() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(Plug(seen.clone()))]);
    let (mut player, _out) = session_with_outbound(&world, 1);

    player.handle_packet(Serverbound::SetCarriedItem(1)).await;
    assert_eq!(player.inventory.selected(), 2);
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0], ("first", Serverbound::SetCarriedItem(1)));
        assert_eq!(seen[1], ("second", Serverbound::SetCarriedItem(2)));
    }

    player.handle_packet(Serverbound::SetCarriedItem(5)).await;
    assert_eq!(player.inventory.selected(), 2);
    assert_eq!(seen.lock().unwrap().len(), 3);

    player.handle_packet(Serverbound::Chat("kick".into())).await;
    assert!(player.disconnected);
    world.shutdown().await;
}
