//! Ranged weapons in regions: firework rockets launched from a clicked
//! block.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::Item;

use crate::projectile::firework::FireworkRocket;
use tuff_core::world::entity::Culprit;
use tuff_core::world::messages::{ActionOutcome, Place, PlayerUpdate};
use tuff_core::world::physics;
use tuff_core::world::projectile::Owner;
use tuff_core::world::region_api::{RegionHooks, RegionWorld};
use tuff_item::item::{ItemStack, MintReason};

/// How far off the clicked face a rocket starts.
const LAUNCH_OFFSET: f64 = 0.15;

pub struct RangedRegionHooks;

impl RegionHooks for RangedRegionHooks {
    fn player_moved(&self, world: &mut dyn RegionWorld, update: &PlayerUpdate) {
        if !update.looks.spin_attack || !update.alive || update.spectator {
            return;
        }
        let [x, y, z] = update.position;
        let half = tuff_core::world::PLAYER_HALF_WIDTH;
        let player = [
            x - half,
            y,
            z - half,
            x + half,
            y + update.looks.height,
            z + half,
        ];
        let touched = world.entity_ids().into_iter().find(|id| {
            world.entity(*id).is_some_and(|tracked| {
                tracked.entity.living().is_some_and(|l| l.alive)
                    && physics::intersects(player, tracked.entity.bounds())
            })
        });
        let touched = touched.or_else(|| {
            world
                .present_players()
                .into_iter()
                .filter(|p| p.id != update.handle.id)
                .find(|p| {
                    let [px, py, pz] = p.position;
                    let other = [
                        px - half,
                        py,
                        pz - half,
                        px + half,
                        py + p.height,
                        pz + half,
                    ];
                    physics::intersects(player, other)
                })
                .map(|p| p.id)
        });
        if let Some(target) = touched {
            update
                .handle
                .send(tuff_core::world::messages::PlayerEvent::SpinAttackTouched { target });
        }
    }

    fn use_item_on(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        _state: BlockState,
    ) -> Option<ActionOutcome> {
        if place.held != Some(Item::FireworkRocket) {
            return None;
        }
        let (dx, dy, dz) = place.face.normal();
        let target = place.target;
        let position = [
            f64::from(target.x) + f64::from(place.cursor[0]) + f64::from(dx) * LAUNCH_OFFSET,
            f64::from(target.y) + f64::from(place.cursor[1]) + f64::from(dy) * LAUNCH_OFFSET,
            f64::from(target.z) + f64::from(place.cursor[2]) + f64::from(dz) * LAUNCH_OFFSET,
        ];
        let stack = ItemStack::mint(Item::FireworkRocket, 1, MintReason::CrossActor)
            .with_components(place.components.clone());
        let owner = Owner::of(
            Culprit::player(place.player.id, place.uuid, place.name.clone()),
            Some(place.player.clone()),
        );
        let id = world.shared().next_entity_id();
        let rocket = FireworkRocket::launched(id, position, &stack, owner, world.rng());
        world.add_entity(Box::new(rocket));
        Some(ActionOutcome::Placed {
            off_hand: place.off_hand,
            item: Item::FireworkRocket,
            placement: None,
        })
    }
}
