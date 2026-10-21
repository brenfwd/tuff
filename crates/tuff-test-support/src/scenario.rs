#![allow(unused_imports)]
use crate::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tuff_core::auth::GameProfile;
use tuff_core::protocol::configuration::ClientInformation;
use tuff_core::protocol::play::Serverbound;
use tuff_core::world::entity::{EntityAttack, EntityHit, EntityInteraction};
use tuff_core::world::inventory::Inventory;
use tuff_core::world::messages::*;
use tuff_core::world::region_api::RegionTask;
use tuff_core::world::session::PlayerSession;
use tuff_core::world::*;
use tuff_core::*;
use tuff_feature_builtin_vanilla_mobs::Mob;
use tuff_feature_builtin_vanilla_mobs::warden::Warden;
use tuff_item::item::Enchantments;
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::entity::Pose;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{Attribute, EntityType};

pub const SKY: i32 = 150;

/// A stone floor at `SKY` over `min..=max` (x and z), walled with a fence
/// ring so nothing wanders off.
pub fn pen(min: i32, max: i32) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in min - 1..=max + 1 {
        for z in min - 1..=max + 1 {
            blocks.push((BlockPos::new(x, SKY, z), Block::GrassBlock.default_state()));
            if x < min || x > max || z < min || z > max {
                blocks.push((BlockPos::new(x, SKY + 1, z), Block::Stone.default_state()));
                blocks.push((BlockPos::new(x, SKY + 2, z), Block::Stone.default_state()));
            }
        }
    }
    blocks
}

pub async fn summon(region: &RegionHandle, kind: EntityType, nbt: Compound, position: [f64; 3]) {
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: nbt.with("id", kind.name()),
            position,
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap(), "{} loads from NBT", kind.name());
}

pub async fn snapshots(region: &RegionHandle) -> Vec<EntitySnapshot> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer.await.unwrap()
}

pub async fn mobs(region: &RegionHandle, kind: EntityType) -> Vec<EntitySnapshot> {
    let mut found: Vec<EntitySnapshot> = snapshots(region)
        .await
        .into_iter()
        .filter(|s| s.kind == kind)
        .collect();
    found.sort_by_key(|s| s.id);
    found
}

pub fn track_of(kind: EntityType) -> impl FnMut(PlayerEvent) -> Option<Track> {
    move |event| match event {
        PlayerEvent::Track(track) if track.player.is_none() && track.kind == kind => Some(track),
        _ => None,
    }
}

/// Waits, ticking, until `check` holds for the region's entities.
pub async fn wait_for(
    world: &TestWorld,
    region: &RegionHandle,
    ticks: u64,
    mut check: impl FnMut(&[EntitySnapshot]) -> bool,
) -> Vec<EntitySnapshot> {
    for _ in 0..ticks {
        let found = snapshots(region).await;
        if check(&found) {
            return found;
        }
        world.ticks(1).await;
    }
    let found = snapshots(region).await;
    assert!(check(&found), "never happened; entities: {found:#?}");
    found
}

pub fn interaction(player: &PlayerHandle, item: Option<Item>) -> EntityInteraction {
    EntityInteraction {
        player: player.id,
        player_uuid: 77,
        position: [0.0; 3],
        location: [0.0; 3],
        stack: item
            .map(|item| tuff_item::item::ItemSnapshot::describe(item, 16, Default::default())),
        off_hand: false,
        creative: false,
        sneaking: false,
        riding: false,
        hero: None,
    }
}

pub async fn interact(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    target: i32,
    item: Item,
) -> HandEffect {
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target,
            interaction: interaction(player, Some(item)),
        })
        .await;
    expect(events, |event| match event {
        PlayerEvent::EntityInteracted { effect, .. } => Some(effect),
        _ => None,
    })
    .await
}

pub fn player_update(
    player: &PlayerHandle,
    position: [f64; 3],
    held: Option<Item>,
) -> RegionRequest {
    RegionRequest::Player(PlayerUpdate {
        landed: None,
        handle: player.clone(),
        uuid: 77,
        name: "Tester".into(),
        seq: 1,
        position,
        swept_from: position,
        chunk: ChunkPos::of_position(position),
        alive: true,
        spectator: false,
        gamemaster: false,
        explosion_knockback_resistance: 0.0,
        creative_flying: false,
        grounded: false,
        feet: None,
        looks: PlayerLooks {
            held: [held, None],
            ..Default::default()
        },
        spawn: Arc::from(Vec::new()),
        update: Arc::from(Vec::new()),
        head: None,
        moves: None,
    })
}

pub fn attack(damage: f32, weapon: Option<ItemStack>) -> EntityAttack {
    EntityAttack {
        attacker: 100,
        attacker_uuid: 77,
        attacker_position: [8.5, SKY as f64 + 1.0, 3.5],
        attacker_yaw: 0.0,
        attacker_pitch: 0.0,
        damage,
        knockback: 0.0,
        weapon: weapon.as_ref().map(tuff_item::item::ItemSnapshot::of),
        critical: false,
        magic: false,
        sweep: false,
        part: None,
    }
}

pub fn stored_entities(dir: &Path, chunk: ChunkPos) -> Vec<Compound> {
    stored_chunk(dir, chunk, true)
        .and_then(|root| root.get_list("Entities").map(<[Tag]>::to_vec))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|tag| tag.as_compound().cloned())
        .collect()
}

pub fn allow_spawning(world: &TestWorld) {
    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "spawn_mobs",
                tuff_core::world::game_rules::RuleValue::Bool(true),
            )
            .expect("a boolean rule");
    });
}

pub fn around(center: ChunkPos, radius: i32) -> Vec<ChunkPos> {
    (-radius..=radius)
        .flat_map(|dx| {
            (-radius..=radius).map(move |dz| ChunkPos::new(center.x + dx, center.z + dz))
        })
        .collect()
}

pub async fn bucket_outcome(
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    seq: i32,
) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

/// A filled bucket used straight down onto `floor`, placing its fluid (and
/// mob, if any) in the air block above it.
pub fn place_bucket(
    player: &PlayerHandle,
    floor: BlockPos,
    item: Item,
    mob: Option<EntityType>,
    mob_data: Option<Compound>,
    sequence: i32,
) -> RegionRequest {
    let eye = [
        floor.x as f64 + 0.5,
        floor.y as f64 + 2.0,
        floor.z as f64 + 0.5,
    ];
    RegionRequest::UseBucket(BucketUse {
        player: player.clone(),
        position: [eye[0], eye[1] - 1.62, eye[2]],
        eye,
        look: tuff_core::world::raycast::view_vector(0.0, 90.0),
        reach: 4.5,
        off_hand: false,
        item,
        fluid: Some(Block::Water),
        mob,
        mob_data,
        mob_components: None,
        sneaking: false,
        creative: false,
        sequence,
        hit: None,
    })
}

pub fn bucket_entity_data(stack: &ItemStack) -> Option<Compound> {
    match stack.components.other.get("minecraft:bucket_entity_data") {
        Some(Tag::Compound(nbt)) => Some(nbt.clone()),
        _ => None,
    }
}

pub async fn mob_health(region: &RegionHandle, id: i32) -> f32 {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    let nbt = answer.await.unwrap().expect("the mob is still there");
    nbt.get("Health")
        .and_then(|t| t.as_float())
        .expect("mobs save their health")
}

/// A pen with a stone roof, so undead stay out of the sun.
pub fn roofed_pen(min: i32, max: i32) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = pen(min, max);
    for x in min - 1..=max + 1 {
        for z in min - 1..=max + 1 {
            blocks.push((BlockPos::new(x, SKY + 4, z), Block::Stone.default_state()));
        }
    }
    blocks
}

pub fn player_at(player: &PlayerHandle, position: [f64; 3], looks: PlayerLooks) -> RegionRequest {
    RegionRequest::Player(PlayerUpdate {
        landed: None,
        handle: player.clone(),
        uuid: 77,
        name: "Tester".into(),
        seq: 1,
        position,
        swept_from: position,
        chunk: ChunkPos::of_position(position),
        alive: true,
        spectator: false,
        gamemaster: false,
        explosion_knockback_resistance: 0.0,
        creative_flying: false,
        grounded: false,
        feet: None,
        looks,
        spawn: Arc::from(Vec::new()),
        update: Arc::from(Vec::new()),
        head: None,
        moves: None,
    })
}

/// Waits for an event `pick` accepts, ticking the world meanwhile.
pub async fn ticking_expect<T>(
    world: &TestWorld,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    ticks: u64,
    mut pick: impl FnMut(PlayerEvent) -> Option<T>,
) -> T {
    for _ in 0..ticks {
        while let Ok(event) = events.try_recv() {
            if let Some(found) = pick(event) {
                return found;
            }
        }
        world.ticks(1).await;
    }
    panic!("nothing within {ticks} ticks");
}

/// Waits for a mob's hit on the player, ticking the world meanwhile.
pub async fn next_hit(
    world: &TestWorld,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    ticks: u64,
) -> EntityHit {
    ticking_expect(world, events, ticks, |event| match event {
        PlayerEvent::EntityHurt(hit) => Some(*hit),
        _ => None,
    })
    .await
}

/// An entity's raw saved NBT, read live rather than from disk.
pub async fn entity_nbt(region: &RegionHandle, id: i32) -> Option<Compound> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap()
}

pub fn in_gold() -> PlayerLooks {
    PlayerLooks {
        armor: [Some(Item::GoldenBoots), None, None, None],
        ..Default::default()
    }
}

/// A session and the packets it writes to its own client.
pub fn session_with_outbound(
    world: &TestWorld,
    id: i32,
) -> (PlayerSession, mpsc::UnboundedReceiver<bytes::Bytes>) {
    session_with_info(world, id, ClientInformation::default())
}

/// Decodes every framed Update Attributes packet's entries (attribute,
/// base value; the server always sends zero modifiers, the client uses
/// the base as the final value) into one map.
pub fn synced_attribute_values(frames: &[bytes::Bytes]) -> HashMap<Attribute, f64> {
    let id = tuff_vanilla_data::packet::play::Clientbound::UpdateAttributes.id();
    let mut values = HashMap::new();
    for frame in frames {
        let mut r = framed_payload(frame);
        if r.read_var_int().unwrap() != id {
            continue;
        }
        r.read_var_int().unwrap();
        let count = r.read_var_int().unwrap();
        for _ in 0..count {
            let attribute = Attribute::from_id(r.read_var_int().unwrap() as u32).unwrap();
            let base = r.read_double().unwrap();
            let modifiers = r.read_var_int().unwrap();
            for _ in 0..modifiers {
                r.read_string().unwrap();
                r.read_double().unwrap();
                r.read_unsigned_byte().unwrap();
            }
            values.insert(attribute, base);
        }
    }
    values
}

/// Summons a mob as `/summon` does, set up as a fresh spawn.
pub async fn summon_fresh(region: &RegionHandle, kind: EntityType, position: [f64; 3]) {
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new().with("id", kind.name()),
            position,
            fresh: true,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap(), "{} summons", kind.name());
}

pub fn session_with_info(
    world: &TestWorld,
    id: i32,
    info: ClientInformation,
) -> (PlayerSession, mpsc::UnboundedReceiver<bytes::Bytes>) {
    let (outbound, outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(id);
    let (_packets, packets_rx) = mpsc::channel(8);
    let profile = GameProfile {
        id: id as u128,
        name: format!("Player{id}"),
        properties: Vec::new(),
    };
    let mut session = PlayerSession::new(
        world.shared.clone(),
        id,
        profile,
        info,
        outbound,
        handle,
        events,
        packets_rx,
        PermissionLevel::Owners,
    );
    session.rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(id as u64);
    (session, outbound_rx)
}

/// The ids of the lasting effects in a mob's saved data.
pub fn effect_ids(nbt: &Compound) -> Vec<String> {
    nbt.get_list("active_effects")
        .unwrap_or(&[])
        .iter()
        .filter_map(Tag::as_compound)
        .filter_map(|effect| effect.get_str("id").map(str::to_owned))
        .collect()
}

/// Runs `f` on the Warden `id` in its region and returns what it gives.
pub async fn with_warden<T: Send + 'static>(
    region: &RegionHandle,
    id: i32,
    f: impl FnOnce(&mut Mob<Warden>) -> T + Send + 'static,
) -> Option<T> {
    let (reply, answer) = oneshot::channel();
    let at = BlockPos::new(0, SKY, 0);
    region
        .request(RegionRequest::WhenLoaded {
            min: at,
            max: at,
            run: RegionTask::new(move |world| {
                let found = world
                    .entity_mut(id)
                    .and_then(|t| t.entity.downcast_mut::<Mob<Warden>>())
                    .map(f);
                let _ = reply.send(found);
            }),
        })
        .await;
    answer.await.unwrap()
}

pub async fn blocks(region: &RegionHandle, op: BlockOp) -> BlockOpResult {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Blocks { op, reply }).await;
    answer.await.unwrap()
}

pub async fn vehicle_of_at(region: &RegionHandle, kind: EntityType, at: BlockPos) -> Option<i32> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: at,
            max: at,
            run: tuff_core::world::region_api::RegionTask::new(move |world| {
                let found = world.entity_ids().into_iter().find_map(|id| {
                    let base = world.entity(id)?.entity.base();
                    (base.kind == kind).then(|| base.vehicle.as_ref().map(|m| m.id))
                });
                let _ = reply.send(found.flatten());
            }),
        })
        .await;
    answer.await.unwrap()
}
