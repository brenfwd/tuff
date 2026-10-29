//! Attack and damage filters plugins declare.

use super::*;
use crate::protocol::play::Serverbound;
use crate::world::entity::Culprit;
use crate::world::messages::PlayerEvent;
use crate::world::messages::RegionRequest;
use crate::world::session::DamageSource;
use tuff_plugin_wasm::combat::{Attacks, Damage};
use tuff_plugin_wasm::wit::bindings::types::{AttackFilter, DamageFilter};
use tuff_world_model::pos::{ChunkPos, RegionPos};

fn hit(kind: &'static tuff_vanilla_data::damage_type::DamageType) -> DamageSource {
    DamageSource {
        kind,
        attacker: Some(Culprit::player(9, 9, "Alex")),
        projectile: None,
        position: None,
        weapon: None,
        direct: true,
    }
}

fn damage_filter(cause: &str, scale: f32) -> DamageFilter {
    DamageFilter {
        victim: None,
        causes: vec![cause.into()],
        attacker: None,
        attacker_kind: None,
        dimension: None,
        area: None,
        scale,
    }
}

fn referee() -> Vec<u8> {
    referee_of("zombie")
}

fn referee_of(kind: &str) -> Vec<u8> {
    let mut data = Data::new();
    let attack = AttackFilter {
        kind: Some(kind.into()),
        tag: None,
        held_item: None,
        attacker: None,
        dimension: None,
        area: None,
    };
    let kinds = EventKinds::PLAYER_ATTACK | EventKinds::DAMAGE_FILTERED;
    let subscribe = data.call("subscribe", &[kinds.to_value()]);
    let intercept_attack = data.call("intercept-attack", &[attack.to_value()]);
    let cancel = data.call(
        "intercept-damage",
        &[damage_filter("generic", 0.0).to_value()],
    );
    let halve = data.call(
        "intercept-damage",
        &[damage_filter("minecraft:player_attack", 0.5).to_value()],
    );
    let bad = data.call(
        "intercept-damage",
        &[damage_filter("generic", -1.0).to_value()],
    );
    let body = format!(
        "{}
         (func {} (local $a i32) {})
         (func {})
         {}",
        register(
            &mut data,
            &[],
            &format!("{subscribe} {intercept_attack} {cancel} {halve} {bad}")
        ),
        export("run-command"),
        ok_code("(i32.const 0)"),
        export("on-event"),
        data.wat()
    );
    module(&plain("referee"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn damage_filters_cancel_and_scale_hits_on_players() {
    let dir = TestDir::new();
    install(dir.path(), "referee.wasm", &referee());
    let world = start(dir.path()).await;
    let client = Client::new(&world);
    let snapshot = host(&world).snapshot();
    assert_eq!(snapshot.attack_filters.len(), 1);
    assert_eq!(
        snapshot.damage_filters.len(),
        2,
        "a negative scale is refused"
    );

    let generic = hit(&tuff_vanilla_data::damage_type::GENERIC);
    assert!(!Damage.allows_player_hurt(&client.session, &generic));
    let melee = hit(&tuff_vanilla_data::damage_type::PLAYER_ATTACK);
    assert!(Damage.allows_player_hurt(&client.session, &melee));
    assert_eq!(Damage.player_hurt_scale(&client.session, &melee), 0.5);
    let other = hit(&tuff_vanilla_data::damage_type::FALL);
    assert!(Damage.allows_player_hurt(&client.session, &other));
    assert_eq!(Damage.player_hurt_scale(&client.session, &other), 1.0);
    world.shutdown().await;
}

async fn spawn_in(
    world: &TestWorld,
    dimension: crate::world::dimension::DimensionId,
    id: &str,
    at: [f64; 3],
) -> crate::world::messages::EntitySnapshot {
    use tuff_plugin_wasm::entities::{self, EntityOp};
    let (nbt, fresh) = entities::fresh_nbt(id, None, rand::random());
    entities::apply(
        &world.shared,
        vec![EntityOp::Spawn {
            dimension,
            nbt,
            position: at,
            fresh,
        }],
    )
    .await;
    world.ticks(6).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let (reply, answer) = tokio::sync::oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let found = answer.await.unwrap();
    found
        .into_iter()
        .find(|e| e.kind.name() == id)
        .expect("spawned")
}

async fn show(client: &mut Client, victim: &crate::world::messages::EntitySnapshot) {
    let position = victim.position;
    client
        .session
        .view
        .subscribed
        .insert(ChunkPos::of_position(position));
    client
        .session
        .on_event(PlayerEvent::Track(crate::world::messages::Track {
            entity: victim.id,
            kind: victim.kind,
            dimension: client.session.dimension,
            chunk: ChunkPos::of_position(position),
            seq: 1,
            position,
            range: 128,
            spawn: crate::world::packets([]),
            update: crate::world::packets([]),
            moves: None,
            head: None,
            player: None,
            spectator: false,
            shape: None,
        }))
        .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attack_filters_cancel_attacks_on_matching_spawned_entities() {
    let dir = TestDir::new();
    install(dir.path(), "referee.wasm", &referee());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let dimension = client.session.dimension;
    let zombie = spawn_in(&world, dimension, "minecraft:zombie", [3.5, 80.0, 3.5]).await;
    let cow = spawn_in(&world, dimension, "minecraft:cow", [5.5, 80.0, 5.5]).await;
    show(&mut client, &zombie).await;
    show(&mut client, &cow).await;
    assert_eq!(
        Attacks
            .intercept(&mut client.session, &Serverbound::Attack(zombie.id))
            .await,
        Verdict::Cancel
    );
    assert_eq!(
        Attacks
            .intercept(&mut client.session, &Serverbound::Attack(cow.id))
            .await,
        Verdict::Allow
    );
    world.shutdown().await;
}

fn swing(damage: f32) -> crate::world::entity::EntityAttack {
    crate::world::entity::EntityAttack {
        attacker: 7,
        attacker_uuid: 77,
        attacker_position: [3.5, 80.0, 1.5],
        attacker_yaw: 0.0,
        attacker_pitch: 0.0,
        damage,
        knockback: 0.0,
        weapon: None,
        critical: false,
        magic: false,
        sweep: false,
        part: None,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn damage_filters_scale_melee_hits_on_mobs() {
    let dir = TestDir::new();
    install(dir.path(), "referee.wasm", &referee());
    let world = start(dir.path()).await;
    let client = Client::new(&world);
    let cow = spawn_in(
        &world,
        client.session.dimension,
        "minecraft:cow",
        [3.5, 80.0, 3.5],
    )
    .await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let (attacker, _events) = PlayerHandle::new(7);
    region
        .request(RegionRequest::AttackEntity {
            attacker,
            target: cow.id,
            attack: swing(4.0),
            sweep: None,
        })
        .await;
    world.ticks(2).await;
    let data = tuff_plugin_wasm::entities::data(world.shared.clone(), cow.uuid)
        .await
        .unwrap();
    let health = data.to_string();
    assert!(health.contains("Health:8"), "half of 4 damage: {health}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attack_filters_cancel_pvp_attacks_found_in_the_player_index() {
    use crate::world::messages::PlayerLooks;
    use crate::world::nearby::{IndexedPlayer, PlayerIndex};
    let dir = TestDir::new();
    install(dir.path(), "referee.wasm", &referee_of("player"));
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let dimension = client.session.dimension;
    let at = [3.5, 80.0, 3.5];
    let (handle, _events) = PlayerHandle::new(4000);
    world.players.send_replace(Arc::new(PlayerIndex::build([(
        dimension,
        IndexedPlayer {
            handle: handle.clone(),
            uuid: 4242,
            name: Arc::from("Steve"),
            position: at,
            looks: PlayerLooks::default(),
            time_since_rest: 0,
        },
    )])));
    client
        .session
        .view
        .subscribed
        .insert(ChunkPos::of_position(at));
    client
        .session
        .on_event(PlayerEvent::Track(crate::world::messages::Track {
            entity: 5000,
            kind: tuff_vanilla_data::registry::EntityType::Player,
            dimension,
            chunk: ChunkPos::of_position(at),
            seq: 1,
            position: at,
            range: 512,
            spawn: crate::world::packets([]),
            update: crate::world::packets([]),
            moves: None,
            head: None,
            player: Some(handle),
            spectator: false,
            shape: None,
        }))
        .await;
    assert_eq!(
        Attacks
            .intercept(&mut client.session, &Serverbound::Attack(5000))
            .await,
        Verdict::Cancel
    );
    world.shutdown().await;
}

const GOLEM: &str = "test:golem";

struct Golems;

impl tuff_core::feature::Feature for Golems {
    fn id(&self) -> &'static str {
        "test-custom-entities"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        use tuff_core::custom::CustomEntityDef;
        use tuff_core::custom::entities::EntityPart;
        use tuff_vanilla_data::registry::EntityType;
        if cx.phase() != tuff_item::load::Phase::Content {
            return Ok(());
        }
        let def = CustomEntityDef::new(GOLEM, EntityType::IronGolem)
            .with_part(EntityPart::hitbox(1.5, 3.0));
        cx.registries().custom.register_entity(self.id(), def)
    }
}

fn attack_spy() -> Vec<u8> {
    let mut data = Data::new();
    let subscribe = data.call("subscribe", &[EventKinds::PLAYER_ATTACK.to_value()]);
    let body = format!(
        "{}
         (func {} (local $a i32) {})
         (func {} (param $ev i32)
           (call $clear)
           (call $put (i32.load offset=48 (local.get $ev)) (i32.load offset=52 (local.get $ev)))
           (call $put_char (i32.const 32))
           (if (i32.load8_u offset=56 (local.get $ev))
             (then (call $put (i32.load offset=60 (local.get $ev)) (i32.load offset=64 (local.get $ev))))
             (else (call $put_char (i32.const 45))))
           {SHOUT})
         {}",
        register(&mut data, &[], &subscribe),
        export("run-command"),
        ok_code("(i32.const 0)"),
        export("on-event").replace(" (param i32)", ""),
        data.wat()
    );
    module(&plain("attack-spy"), &body)
}

async fn start_with_golems(dir: &Path) -> TestWorld {
    let plugin = WasmPluginsFeature {
        directory: Some(plugins_dir(dir)),
        limits: Limits {
            fuel: 2_000_000,
            ..Limits::default()
        },
    };
    let world = TestWorld::start_with_features(dir, vec![Box::new(Golems), Box::new(plugin)]);
    host(&world).sync().await;
    world
}

async fn spawn_golem(world: &TestWorld) -> Vec<crate::world::messages::EntitySnapshot> {
    let at = [8.5, 100.0, 8.5];
    let mut nbt = world
        .shared
        .custom
        .spawn_entity_nbt(GOLEM, at, 0.0)
        .expect("registered");
    nbt.insert("NoAI", true);
    let region = world.region(RegionPos::new(0, 0)).await;
    let (reply, summoned) = tokio::sync::oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt,
            position: at,
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    world.ticks(3).await;
    let (reply, answer) = tokio::sync::oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer.await.unwrap()
}

async fn attack_report(
    world: &TestWorld,
    client: &mut Client,
    target: &crate::world::messages::EntitySnapshot,
) -> Vec<String> {
    show(client, target).await;
    assert_eq!(
        Attacks
            .intercept(&mut client.session, &Serverbound::Attack(target.id))
            .await,
        Verdict::Allow
    );
    announced(world).await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn custom_entity_attack_on_the_part_reports_the_owner() {
    use tuff_vanilla_data::registry::EntityType;
    let dir = TestDir::new();
    install(dir.path(), "spy.wasm", &attack_spy());
    let world = start_with_golems(dir.path()).await;
    let mut client = Client::new(&world);
    let all = spawn_golem(&world).await;
    let part = all
        .iter()
        .find(|e| e.kind == EntityType::Interaction)
        .expect("the interaction part");
    assert_eq!(
        attack_report(&world, &mut client, part).await,
        ["minecraft:iron_golem test:golem"]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn custom_entity_attack_on_the_stand_in_reports_custom_id() {
    use tuff_vanilla_data::registry::EntityType;
    let dir = TestDir::new();
    install(dir.path(), "spy.wasm", &attack_spy());
    let world = start_with_golems(dir.path()).await;
    let mut client = Client::new(&world);
    let all = spawn_golem(&world).await;
    let golem = all
        .iter()
        .find(|e| e.kind == EntityType::IronGolem)
        .expect("the stand-in");
    assert_eq!(
        attack_report(&world, &mut client, golem).await,
        ["minecraft:iron_golem test:golem"]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn custom_entity_attack_on_a_plain_entity_reports_no_custom() {
    let dir = TestDir::new();
    install(dir.path(), "spy.wasm", &attack_spy());
    let world = start_with_golems(dir.path()).await;
    let mut client = Client::new(&world);
    let cow = spawn_in(
        &world,
        client.session.dimension,
        "minecraft:cow",
        [3.5, 80.0, 3.5],
    )
    .await;
    assert_eq!(
        attack_report(&world, &mut client, &cow).await,
        ["minecraft:cow -"]
    );
    world.shutdown().await;
}
