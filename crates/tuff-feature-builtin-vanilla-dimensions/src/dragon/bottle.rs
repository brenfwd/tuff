//! Bottling dragon's breath: a glass bottle held near a cloud the dragon
//! breathed.

use rand::Rng;
use tokio::sync::oneshot;
use tuff_core::world::game_event::{Context, GameEvent, Source};
use tuff_core::world::raycast::{self, FluidMode};
use tuff_core::world::region_api::RegionTask;
use tuff_item::item::{ItemStack, MintReason, PotionContents};
use tuff_vanilla_data::registry::{Potion, SoundEvent};
use tuff_vanilla_data::{Block, Item};

use crate::dragon::fight::FightsExt;
use tuff_core::protocol::play::{self, GameMode};
use tuff_core::world::entity::RegionEntity;
use tuff_core::world::messages::{HandEffect, RegionHandle, RegionRequest};
use tuff_core::world::region_api::RegionWorld;
use tuff_core::world::session::PlayerSession;
use tuff_core::world::{framed, packets};
use tuff_vanilla_common::effect_cloud::AreaEffectCloud;
use tuff_world_model::BlockPos;

/// Use Item with a glass bottle in `off_hand`: fills it from a cloud of
/// dragon's breath within two blocks of the player.
pub async fn use_bottle(session: &mut PlayerSession, off_hand: bool) {
    const REACH: f64 = 2.0;
    if session.is_dead()
        || session.mode.game_mode == GameMode::Spectator
        || session.inventory.hand(off_hand).map(|s| s.item()) != Some(Item::GlassBottle)
        || session.on_cooldown(Item::GlassBottle)
    {
        return;
    }
    let [x, y, z] = session.pose.position;
    let half = tuff_core::world::PLAYER_HALF_WIDTH + REACH;
    let bounds = [
        x - half,
        y - REACH,
        z - half,
        x + half,
        y + session.height() + REACH,
        z + half,
    ];
    let Some(region) = session.region_of(BlockPos::new(
        x.floor() as i32,
        y.floor() as i32,
        z.floor() as i32,
    )) else {
        return;
    };
    let (reply, answer) = oneshot::channel();
    region.request(bottle(bounds, reply)).await;
    if !answer.await.unwrap_or(false) {
        fill_from_water(session, off_hand, &region).await;
        return;
    }
    let packet = packets([framed(&play::Sound {
        sound_event: SoundEvent::ItemBottleFillDragonbreath.id(),
        category: play::sound_category::NEUTRAL,
        position: session.pose.position,
        volume: 1.0,
        pitch: 1.0,
        seed: session.rng.r#gen(),
    })]);
    session.broadcast_chunk(packet).await;
    session
        .apply_hand_effect(off_hand, HandEffect::Exchange(Item::DragonBreath))
        .await;
    session.send_inventory();
}

async fn fill_from_water(session: &mut PlayerSession, off_hand: bool, region: &RegionHandle) {
    let eye = session.eye();
    let look = tuff_core::world::raycast::view_vector(session.pose.yaw, session.pose.pitch);
    let reach = tuff_core::world::session::BLOCK_INTERACTION_RANGE;
    let (reply, answer) = oneshot::channel();
    let actor = Source::player(session.conn.handle.id).sneaking(session.pose.sneaking);
    region
        .request(water_scoop(eye, look, reach, actor, reply))
        .await;
    if !answer.await.unwrap_or(false) {
        return;
    }
    let packet = packets([framed(&play::Sound {
        sound_event: SoundEvent::ItemBottleFill.id(),
        category: play::sound_category::NEUTRAL,
        position: session.pose.position,
        volume: 1.0,
        pitch: 1.0,
        seed: session.rng.r#gen(),
    })]);
    session.broadcast_chunk(packet).await;
    session
        .apply_hand_effect(
            off_hand,
            HandEffect::ExchangeStack(Box::new(water_bottle())),
        )
        .await;
    session.send_inventory();
}

fn water_bottle() -> ItemStack {
    let mut stack = ItemStack::mint(Item::Potion, 1, MintReason::Transform);
    stack.components.potion_contents = Some(Box::new(PotionContents {
        potion: Some(Potion::Water),
        ..PotionContents::default()
    }));
    stack
}

/// Casts the view ray through a source-only fluid pass and answers whether
/// it met a water source the player may fill a bottle from.
pub fn water_scoop(
    eye: [f64; 3],
    look: [f64; 3],
    reach: f64,
    actor: Source,
    reply: oneshot::Sender<bool>,
) -> RegionRequest {
    RegionRequest::Run(RegionTask::new(move |world| {
        let _ = reply.send(scoop_water(world, eye, look, reach, actor));
    }))
}

/// Whether the view ray from `eye` meets a water source within `reach`.
fn scoop_water(
    world: &mut dyn RegionWorld,
    eye: [f64; 3],
    look: [f64; 3],
    reach: f64,
    actor: Source,
) -> bool {
    let to = [0, 1, 2].map(|i| eye[i] + look[i] * reach);
    let air = Block::Air.default_state();
    let level = |pos: BlockPos| world.block(pos).unwrap_or(air);
    let Some(hit) = raycast::cast(eye, to, FluidMode::SourceOnly, &level) else {
        return false;
    };
    let state = level(hit.pos);
    let water = tuff_datapack::tags::is(
        tuff_vanilla_data::tag::fluid::key::WATER,
        state.fluid_state().fluid(),
    );
    if !water || !world.dimension_info().contains_block(hit.pos) {
        return false;
    }
    world.game_event(
        GameEvent::FluidPickup,
        hit.pos.center(),
        Context::from(actor),
    );
    true
}

/// Shrinks the first cloud of dragon's breath touching `bounds` (a glass
/// bottle held near it), answering whether there was one.
pub fn bottle(bounds: [f64; 6], reply: tokio::sync::oneshot::Sender<bool>) -> RegionRequest {
    let corner = |i: usize| {
        tuff_world_model::BlockPos::containing([bounds[i], bounds[i + 1], bounds[i + 2]])
    };
    RegionRequest::WhenLoaded {
        min: corner(0),
        max: corner(3),
        run: RegionTask::new(move |world| {
            let _ = reply.send(bottle_in(world, bounds));
        }),
    }
}

fn bottle_in(world: &mut dyn RegionWorld, bounds: [f64; 6]) -> bool {
    let dragon = world
        .shared()
        .fight(world.dimension_id())
        .and_then(|fight| fight.view().dragon);
    for id in world.entity_ids() {
        let Some(cloud) = world
            .entity_mut(id)
            .and_then(|t| t.entity.downcast_mut::<AreaEffectCloud>())
        else {
            continue;
        };
        let b = cloud.bounds();
        let touches = (0..3).all(|i| b[i] < bounds[i + 3] && b[i + 3] > bounds[i]);
        if touches && cloud.from_dragon(dragon) {
            cloud.bottle();
            return true;
        }
    }
    false
}
