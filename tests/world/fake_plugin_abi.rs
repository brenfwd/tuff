//! The plugin side of fake players: ownership, bad input, cheap state
//! reads and driving a fake with held input and hand actions, on a real
//! server.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tuff::Server;
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::world::Shared;
use tuff_core::world::dimension::Dimensions;
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_item::load::{LoadError, Phase};
use tuff_plugin_wasm::fake::Fakes;
use tuff_plugin_wasm::sandbox::uuid as wit_uuid;
use tuff_plugin_wasm::wit::bindings::types::{
    FakeAction, FakeError, FakeInput, FakeSpec, FakeState, Uuid, Vec3,
};
use tuff_plugin_wasm::world::Env;
use tuff_test_support::TestDir;

type Cell = Arc<OnceLock<Arc<Shared>>>;

struct Capture(Cell);

struct CaptureService(Cell);

impl ServiceFactory for Capture {
    fn create(&self, _: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(CaptureService(self.0.clone()))
    }
}

impl WorldService for CaptureService {
    fn start(&self, shared: &Arc<Shared>) {
        let _ = self.0.set(shared.clone());
    }
}

impl Feature for CaptureSharedFeature {
    fn id(&self) -> &'static str {
        "capture-shared"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries()
                .services
                .push(Arc::new(Capture(self.0.clone())));
        }
        Ok(())
    }
}

struct CaptureSharedFeature(Cell);

struct World {
    handle: tuff::ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    _instance: TestDir,
    shared: Arc<Shared>,
}

async fn start() -> World {
    let instance = TestDir::new();
    let cell = Cell::default();
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(CaptureSharedFeature(cell.clone())));
    let server = Server::builder()
        .features(features)
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();
    let shared = cell.get().expect("the world started").clone();
    World {
        handle,
        task,
        _instance: instance,
        shared,
    }
}

async fn stop(world: World) {
    world.handle.shutdown().await.unwrap();
    world.task.await.unwrap().unwrap();
}

fn spec(name: &str, at: Option<[f64; 3]>) -> FakeSpec {
    FakeSpec {
        name: name.to_owned(),
        uuid: None,
        slim_skin: false,
        dimension: None,
        pos: at.map(|[x, y, z]| Vec3 { x, y, z }),
        rotation: None,
        game_mode: None,
        listed: true,
    }
}

fn walking(forward: i8) -> FakeInput {
    FakeInput {
        forward,
        strafe: 0,
        jump: false,
        sprint: false,
        sneak: false,
        yaw: 0.0,
        pitch: 0.0,
    }
}

/// One plugin's fakes, called from a blocking thread as guest code is.
struct Guest {
    id: &'static str,
    env: Env,
    fakes: Option<Fakes>,
}

impl Guest {
    fn new(world: &World, id: &'static str) -> Self {
        Self {
            id,
            env: Env {
                shared: world.shared.clone(),
                runtime: tokio::runtime::Handle::current(),
            },
            fakes: Some(Fakes::default()),
        }
    }

    async fn call<T: Send + 'static>(
        &mut self,
        f: impl FnOnce(&mut Fakes, &Env, &str) -> T + Send + 'static,
    ) -> T {
        let (mut fakes, env, id) = (self.fakes.take().unwrap(), self.env.clone(), self.id);
        let (fakes, out) = tokio::task::spawn_blocking(move || {
            let out = f(&mut fakes, &env, id);
            (fakes, out)
        })
        .await
        .unwrap();
        self.fakes = Some(fakes);
        out
    }

    async fn spawn(&mut self, spec: FakeSpec) -> Result<Uuid, FakeError> {
        self.call(move |fakes, env, id| fakes.spawn(env, id, spec))
            .await
    }

    async fn state(&mut self, fake: Uuid) -> Option<FakeState> {
        self.call(move |fakes, env, _| fakes.state(env, fake)).await
    }

    async fn act(&mut self, fake: Uuid, action: FakeAction) -> Result<(), FakeError> {
        self.call(move |fakes, env, _| fakes.act(env, fake, action))
            .await
    }

    async fn input(&mut self, fake: Uuid, input: FakeInput) -> Result<(), FakeError> {
        self.call(move |fakes, _, _| fakes.input(fake, input)).await
    }

    /// Ticks until `check` holds on the fake's state.
    async fn until(&mut self, fake: Uuid, check: impl Fn(&FakeState) -> bool) -> FakeState {
        let mut last = None;
        for _ in 0..1200 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let state = self.state(fake).await;
            if let Some(state) = state.clone().filter(&check) {
                return state;
            }
            last = state;
        }
        panic!("the fake never reached the expected state: {last:?}");
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_owner_drives_a_fake_and_bad_calls_are_refused() {
    let world = start().await;
    let mut mine = Guest::new(&world, "owner-a");
    let mut theirs = Guest::new(&world, "owner-b");
    let made = mine.spawn(spec("Alpha", None)).await;
    let alpha = wit_uuid(tuff_core::world::fake::derive_uuid(
        "owner-a",
        "Alpha",
        tuff_core::world::fake::Model::Wide,
    ));
    assert_eq!(made, Ok(alpha));
    assert_eq!(
        mine.spawn(spec("Alpha", None)).await,
        Err(FakeError::NameTaken)
    );
    assert!(matches!(
        mine.spawn(spec("bad name", None)).await,
        Err(FakeError::Invalid(_))
    ));
    let nowhere = FakeSpec {
        dimension: Some("nowhere".into()),
        ..spec("Beta", None)
    };
    assert_eq!(mine.spawn(nowhere).await, Err(FakeError::UnknownDimension));
    assert_eq!(
        theirs.input(alpha, walking(1)).await,
        Err(FakeError::NotOwner)
    );
    assert_eq!(
        theirs.act(alpha, FakeAction::Swing).await,
        Err(FakeError::NotOwner)
    );
    assert_eq!(theirs.state(alpha).await, None);
    assert_eq!(
        mine.act(wit_uuid(1), FakeAction::Swing).await,
        Err(FakeError::NotOwner)
    );
    assert_eq!(
        mine.act(alpha, FakeAction::SelectSlot(9)).await,
        Err(FakeError::Invalid("slot".into()))
    );
    assert_eq!(
        mine.act(alpha, FakeAction::Attack(wit_uuid(2))).await,
        Err(FakeError::UnknownEntity)
    );
    let gone = mine
        .call(move |fakes, env, id| (fakes.remove(env, id, alpha), fakes.uuids()))
        .await;
    assert_eq!(gone, (Ok(()), Vec::new()));
    assert_eq!(mine.state(alpha).await, None);
    stop(world).await;
}

/// A stone floor at y 149 under an air box from y 150 to 153, x and z
/// from -8 to 8, written until it reads back whole: the terrain under a
/// fake's spawn varies per run, so a walk must not depend on it.
async fn flat_arena(world: &World) {
    use tuff_plugin_wasm::world::{BlockWrite, parse_state, read, write};
    let dimension = world
        .shared
        .dimensions
        .by_key("minecraft:overworld")
        .unwrap()
        .id;
    let stone = parse_state("minecraft:stone").unwrap();
    let air = parse_state("minecraft:air").unwrap();
    let mut wanted = Vec::new();
    for x in -8..=8 {
        for z in -8..=8 {
            wanted.push((tuff_world_model::BlockPos { x, y: 149, z }, stone));
            for y in 150..=153 {
                wanted.push((tuff_world_model::BlockPos { x, y, z }, air));
            }
        }
    }
    for _ in 0..400 {
        let positions: Vec<_> = wanted.iter().map(|(pos, _)| *pos).collect();
        let found = read(world.shared.clone(), dimension, positions).await;
        let wrong: Vec<_> = wanted
            .iter()
            .zip(found)
            .filter(|((_, state), found)| *found != Some(*state))
            .map(|((pos, state), _)| BlockWrite {
                dimension,
                pos: *pos,
                state: *state,
                updates: false,
            })
            .collect();
        if wrong.is_empty() {
            return;
        }
        write(world.shared.clone(), wrong).await;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the arena floor never took");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_forward_moves_a_fake_and_an_attack_hurts_another() {
    let world = start().await;
    let mut bots = Guest::new(&world, "bots");
    flat_arena(&world).await;
    let runner = bots
        .spawn(spec("Runner", Some([0.5, 150.0, 0.5])))
        .await
        .unwrap();
    let start = bots.until(runner, |s| s.alive && s.on_ground).await;
    bots.input(runner, walking(1)).await.unwrap();
    let moved = bots
        .until(runner, |s| {
            let (dx, dz) = (s.pos.x - start.pos.x, s.pos.z - start.pos.z);
            dx * dx + dz * dz > 1.0
        })
        .await;
    assert!(moved.velocity.x != 0.0 || moved.velocity.z != 0.0);
    bots.input(runner, walking(0)).await.unwrap();

    let at = [moved.pos.x, moved.pos.y, moved.pos.z];
    // An attack is dropped while the runner's own view has not yet received the
    // victim, so it is repeated until one lands.
    let victim = bots
        .spawn(spec("Victim", Some([at[0] + 1.0, at[1], at[2]])))
        .await
        .unwrap();
    bots.until(victim, |s| s.alive && s.on_ground).await;
    let mut hurt = None;
    for _ in 0..1200 {
        bots.act(runner, FakeAction::Attack(victim)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        hurt = bots.state(victim).await.filter(|s| s.health < 20.0);
        if hurt.is_some() {
            break;
        }
    }
    let hurt = hurt.expect("the attack never landed");
    assert!(hurt.health > 0.0);
    stop(world).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_creative_fake_digs_the_block_below_it() {
    use tuff_plugin_wasm::wit::bindings::types::{BlockPos, Direction, FakeDig, GameMode};
    let world = start().await;
    let mut bots = Guest::new(&world, "diggers");
    let digger = bots
        .spawn(FakeSpec {
            game_mode: Some(GameMode::Creative),
            ..spec("Digger", None)
        })
        .await
        .unwrap();
    let standing = bots.until(digger, |s| s.alive && s.on_ground).await;
    let below = tuff_world_model::BlockPos {
        x: standing.pos.x.floor() as i32,
        y: standing.pos.y.round() as i32 - 1,
        z: standing.pos.z.floor() as i32,
    };
    let dimension = world
        .shared
        .dimensions
        .by_key("minecraft:overworld")
        .unwrap()
        .id;
    let read = || {
        let shared = world.shared.clone();
        async move { tuff_plugin_wasm::world::read(shared, dimension, vec![below]).await[0] }
    };
    assert!(
        read()
            .await
            .is_some_and(|state| state.block() != tuff_vanilla_data::Block::Air)
    );
    let dig = FakeDig {
        pos: BlockPos {
            x: below.x,
            y: below.y,
            z: below.z,
        },
        face: Direction::Up,
    };
    bots.act(digger, FakeAction::Dig(dig)).await.unwrap();
    for _ in 0..400 {
        if read()
            .await
            .is_some_and(|state| state.block() == tuff_vanilla_data::Block::Air)
        {
            stop(world).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the block was never dug");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_all_of_an_owners_fakes_ends_them_and_the_limit_holds() {
    let world = start().await;
    let mut bots = Guest::new(&world, "crowd");
    let mut made = Vec::new();
    for n in 0..tuff_core::world::fake::OWNER_LIMIT {
        made.push(bots.spawn(spec(&format!("C{n}"), None)).await.unwrap());
    }
    assert_eq!(
        bots.spawn(spec("OneTooMany", None)).await,
        Err(FakeError::LimitReached)
    );
    assert_eq!(
        bots.call(|fakes, _, _| fakes.uuids().len()).await,
        made.len()
    );
    let removed = tuff_plugin_wasm::fake::remove(&world.shared, "crowd", None).await;
    assert_eq!(removed, made.len());
    for _ in 0..400 {
        if bots.call(|fakes, _, _| fakes.uuids().is_empty()).await {
            stop(world).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the fakes never left");
}
