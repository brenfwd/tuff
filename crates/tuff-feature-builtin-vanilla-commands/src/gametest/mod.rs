//! Game tests: the `test_instance` and `test_environment` registries, the
//! runner actor and `/test`, plus the headless run the `gametest` server
//! mode uses.

pub mod registry;
pub mod report;
pub mod runner;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{mpsc, oneshot, watch};
use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, ParsedNode};
use tuff_core::world::Shared;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_item::text::{ClickEvent, TextComponent};
use tuff_vanilla_data::Block;

use self::registry::{Rotation, TestRegistry};
use self::runner::{
    Geometry, Reporter, RetryOptions, Run, RunPlan, Summary, TestSpec, WorldAccess, into_batches,
    read_instance_nbt, test_instance_nbt,
};
use super::{Context, Executor, SuggestInput, executor_future};

pub use self::registry::{TestFunction, TestHelper, register_test_function};

const NEARBY_RADIUS: i32 = 15;
const FULL_RADIUS: i32 = 250;
const MAX_CLEAR_RADIUS: i32 = 1024;
const VERIFY_GRID_AXIS: i32 = 10;
const VERIFY_BATCH: usize = 100;
const VERIFY_COPIES: usize = 100;
const TEST_POS_Z_OFFSET: i32 = 3;
const MAX_CREATE_SIZE: i32 = 48;
const REQUESTS: usize = 16;
const PROGRESS_INTERVAL: u64 = 20;

enum Request {
    Start(Box<RunPlan>, oneshot::Sender<()>),
    Stop(oneshot::Sender<()>),
    Failed(oneshot::Sender<Vec<String>>),
    LastSummary(oneshot::Sender<Option<Summary>>),
}

/// The runner actor's handle; one per world.
pub struct GameTests {
    requests: mpsc::Sender<Request>,
    inbox: Mutex<Option<mpsc::Receiver<Request>>>,
    done: watch::Sender<u64>,
}

pub struct GameTestsFactory;

impl ServiceFactory for GameTestsFactory {
    fn create(
        &self,
        _dimensions: &tuff_core::world::dimension::Dimensions,
    ) -> Arc<dyn WorldService> {
        // Bounded: requests come from commands, which wait their turn.
        let (requests, inbox) = mpsc::channel(REQUESTS);
        Arc::new(GameTests {
            requests,
            inbox: Mutex::new(Some(inbox)),
            done: watch::Sender::new(0),
        })
    }
}

impl WorldService for GameTests {
    fn start(&self, shared: &Arc<Shared>) {
        let Some(inbox) = self
            .inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let shared = shared.clone();
        let done = self.done.clone();
        tokio::spawn(actor(shared, inbox, done));
    }

    fn settled(&self, tick: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let mut done = self.done.subscribe();
        Box::pin(async move {
            let _ = done.wait_for(|finished| *finished >= tick).await;
        })
    }
}

async fn actor(shared: Arc<Shared>, mut inbox: mpsc::Receiver<Request>, done: watch::Sender<u64>) {
    let mut clock = shared.clock.clone();
    let member = shared.tick_barrier.join(*clock.borrow());
    done.send_replace(*clock.borrow());
    let mut run: Option<Run> = None;
    let mut failed: std::collections::BTreeSet<String> = Default::default();
    let mut last: Option<Summary> = None;
    loop {
        tokio::select! {
            changed = clock.changed() => {
                if changed.is_err() {
                    break;
                }
                let tick = *clock.borrow_and_update();
                if let Some(active) = run.as_mut() {
                    let contents = shared.datapacks.contents();
                    let registry = TestRegistry::current(&contents);
                    let mut world = WorldAccess::new(&shared);
                    active.tick(&mut world, &registry).await;
                    if active.logs_progress() && tick.is_multiple_of(PROGRESS_INTERVAL) {
                        tuff_core::log::info!("{}", active.progress_bar());
                    }
                    failed.append(&mut active.failed_tests);
                    if active.finished() {
                        last = Some(active.summary());
                        run = None;
                    }
                    world.finish().await;
                }
                member.finished(tick);
                done.send_replace(tick);
            }
            request = inbox.recv() => {
                let Some(request) = request else { break };
                match request {
                    Request::Start(plan, reply) => {
                        let mut world = WorldAccess::new(&shared);
                        if let Some(mut old) = run.take() {
                            old.stop(&mut world).await;
                        }
                        failed.clear();
                        last = None;
                        let contents = shared.datapacks.contents();
                        let registry = TestRegistry::current(&contents);
                        let mut new = Run::new(*plan);
                        new.start(&mut world, &registry).await;
                        failed.append(&mut new.failed_tests);
                        if new.finished() {
                            last = Some(new.summary());
                        } else {
                            run = Some(new);
                        }
                        world.finish().await;
                        let _ = reply.send(());
                    }
                    Request::Stop(reply) => {
                        if let Some(mut old) = run.take() {
                            let mut world = WorldAccess::new(&shared);
                            old.stop(&mut world).await;
                            world.finish().await;
                        }
                        let _ = reply.send(());
                    }
                    Request::Failed(reply) => {
                        let _ = reply.send(failed.iter().cloned().collect());
                    }
                    Request::LastSummary(reply) => {
                        let _ = reply.send(last.clone());
                    }
                }
            }
        }
    }
}

impl GameTests {
    async fn send(&self, request: Request) {
        let _ = self.requests.send(request).await;
    }

    async fn start(&self, plan: RunPlan) {
        let (reply, started) = oneshot::channel();
        self.send(Request::Start(Box::new(plan), reply)).await;
        let _ = started.await;
    }

    async fn stop(&self) {
        let (reply, stopped) = oneshot::channel();
        self.send(Request::Stop(reply)).await;
        let _ = stopped.await;
    }

    async fn failed(&self) -> Vec<String> {
        let (reply, failed) = oneshot::channel();
        self.send(Request::Failed(reply)).await;
        failed.await.unwrap_or_default()
    }
}

/// The summary of the last finished run, `None` while one runs or
/// before any.
pub async fn last_summary(shared: &Shared) -> Option<Summary> {
    let tests = shared.services.get::<GameTests>()?;
    let (reply, summary) = oneshot::channel();
    tests.send(Request::LastSummary(reply)).await;
    summary.await.ok().flatten()
}

/// The ids of the tests that failed since the last run started, which
/// `/test runfailed` runs again.
pub async fn failed_tests(shared: &Shared) -> Vec<String> {
    match shared.services.get::<GameTests>() {
        Some(tests) => tests.failed().await,
        None => Vec::new(),
    }
}

fn service(shared: &Shared) -> Result<&GameTests, CommandError> {
    shared
        .services
        .get::<GameTests>()
        .ok_or_else(|| CommandError::simple("command.failed"))
}

/// What the headless runner should run.
#[derive(Debug, Clone, Default)]
pub struct HeadlessOptions {
    /// A resource selector (`*`/`?` wildcards); `None` runs every test
    /// that is not manual only.
    pub tests: Option<String>,
    /// Runs each selected test 100 times per rotation.
    pub verify: bool,
    pub repeat_count: i32,
    pub report: Option<std::path::PathBuf>,
}

#[derive(Debug)]
pub enum HeadlessError {
    NoTests(String),
    NotRunning,
}

impl std::fmt::Display for HeadlessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTests(selection) => {
                write!(f, "Test selection matcher ({selection}) found no tests")
            }
            Self::NotRunning => f.write_str("the game test runner is not running"),
        }
    }
}

impl std::error::Error for HeadlessError {}

/// Runs the selected tests in the overworld at `origin` and resolves with
/// the summary once every test finished, like the game's test server.
pub async fn run_headless(
    shared: &Arc<Shared>,
    options: HeadlessOptions,
    origin: [i32; 3],
) -> Result<Summary, HeadlessError> {
    let tests = shared
        .services
        .get::<GameTests>()
        .ok_or(HeadlessError::NotRunning)?;
    let contents = shared.datapacks.contents();
    let registry = TestRegistry::current(&contents);
    let ids: Vec<String> = match &options.tests {
        Some(selection) => {
            let patterns: Vec<String> = selection
                .split_whitespace()
                .map(|p| {
                    if p.contains(':') {
                        p.to_owned()
                    } else {
                        format!("minecraft:{p}")
                    }
                })
                .collect();
            let ids: Vec<String> = registry
                .select(&patterns)
                .into_iter()
                .filter(|id| !registry.instances[id].manual_only)
                .collect();
            if ids.is_empty() {
                return Err(HeadlessError::NoTests(selection.clone()));
            }
            ids
        }
        None => registry
            .instances
            .iter()
            .filter(|(_, instance)| !instance.manual_only)
            .map(|(id, _)| id.clone())
            .collect(),
    };
    let mut specs = Vec::new();
    for id in ids {
        let instance = registry.instances[&id].clone();
        let Some(dimension) = shared.dimensions.by_key(&instance.dimension).map(|d| d.id) else {
            tuff_core::log::error!("Missing level for dimension: {}", instance.dimension);
            continue;
        };
        let spec = |rotation| TestSpec {
            id: id.clone(),
            instance: instance.clone(),
            dimension,
            extra_rotation: rotation,
            retry: RetryOptions::NONE,
            test_pos: None,
        };
        if options.tests.is_some() && options.verify {
            for rotation in Rotation::ALL {
                specs.extend(std::iter::repeat_n(spec(rotation), VERIFY_COPIES));
            }
        } else if options.tests.is_some() && options.repeat_count > 0 {
            specs.extend(std::iter::repeat_n(
                spec(Rotation::None),
                options.repeat_count as usize,
            ));
        } else {
            specs.push(spec(Rotation::None));
        }
    }
    let origins = shared.dimensions.iter().map(|d| (d.id, origin)).collect();
    let (done, summary) = oneshot::channel();
    tuff_core::log::info!(
        "{} tests are now running at position {}, {}, {}!",
        specs.len(),
        origin[0],
        origin[1],
        origin[2]
    );
    tests
        .start(RunPlan {
            batches: into_batches(specs, runner::MAX_TESTS_PER_BATCH),
            origins,
            tests_per_row: runner::DEFAULT_TESTS_PER_ROW,
            clear_on_batch: false,
            grid_for_existing: false,
            halt_on_error: false,
            clear_between_batches: false,
            max_per_batch: runner::MAX_TESTS_PER_BATCH,
            reporter: Reporter::Log,
            junit: options.report,
            done: Some(done),
        })
        .await;
    summary.await.map_err(|_| HeadlessError::NotRunning)
}

/// How a `/test` subcommand finds its tests.
enum Finder {
    /// Registry entries the selector picked.
    Selection(Vec<String>),
    /// The tests that failed in the last run.
    Failed {
        only_required: bool,
    },
    Nearest,
    Nearby,
    LookedAt,
    Radius(i32),
}

struct Found {
    instances: Vec<String>,
    positions: Vec<(DimensionId, [i32; 3])>,
}

fn int_arg(ctx: &Context<'_>, name: &str) -> Option<i32> {
    match ctx.arg(name) {
        Some(Value::Int(v)) => Some(*v),
        _ => None,
    }
}

fn bool_arg(ctx: &Context<'_>, name: &str) -> Option<bool> {
    match ctx.arg(name) {
        Some(Value::Bool(v)) => Some(*v),
        _ => None,
    }
}

fn source_block(ctx: &Context<'_>) -> [i32; 3] {
    ctx.source.position.map(|v| v.floor() as i32)
}

fn manhattan(a: [i32; 3], b: [i32; 3]) -> i64 {
    (0..3).map(|i| i64::from((a[i] - b[i]).abs())).sum()
}

fn within(a: [i32; 3], b: [i32; 3], radius: i32) -> bool {
    let d: i64 = (0..3).map(|i| i64::from(a[i] - b[i]).pow(2)).sum();
    d <= i64::from(radius).pow(2)
}

async fn test_blocks_near(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    center: [i32; 3],
    radius: i32,
) -> Vec<[i32; 3]> {
    crate::world::find_test_instances(ctx, dimension, center, radius)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|pos| within(*pos, center, radius))
        .collect()
}

/// A test instance block's data and geometry.
struct Placed {
    pos: [i32; 3],
    data: runner::InstanceData,
    geometry: Geometry,
}

async fn read_placed(
    ctx: &mut Context<'_>,
    registry: &TestRegistry,
    dimension: DimensionId,
    pos: [i32; 3],
) -> Option<Placed> {
    let copied = crate::world::copy_blocks(ctx, dimension, &[pos])
        .await
        .ok()?;
    let (state, nbt) = copied.into_iter().next().flatten()?;
    if state.block() != Block::TestInstanceBlock {
        return None;
    }
    let data = read_instance_nbt(&nbt?)?;
    let instance = data.test.as_ref().and_then(|t| registry.instances.get(t));
    let geometry = Geometry {
        block: pos,
        size: data.size,
        rotation: instance
            .map_or(Rotation::None, |i| i.rotation)
            .then(data.rotation),
        padding: instance.map_or(0, |i| i.padding),
    };
    Some(Placed {
        pos,
        data,
        geometry,
    })
}

fn ray_hits(eye: [f64; 3], look: [f64; 3], low: [i32; 3], high: [i32; 3]) -> bool {
    let (mut t_min, mut t_max) = (0.0f64, 1.0f64);
    for axis in 0..3 {
        let (lo, hi) = (f64::from(low[axis]), f64::from(high[axis] + 1));
        if look[axis].abs() < 1e-9 {
            if eye[axis] < lo || eye[axis] > hi {
                return false;
            }
            continue;
        }
        let a = (lo - eye[axis]) / look[axis];
        let b = (hi - eye[axis]) / look[axis];
        t_min = t_min.max(a.min(b));
        t_max = t_max.min(a.max(b));
        if t_min > t_max {
            return false;
        }
    }
    true
}

fn look_vector(ctx: &Context<'_>, length: f64) -> ([f64; 3], [f64; 3]) {
    let [yaw, pitch] = ctx.source.rotation.map(|v| f64::from(v).to_radians());
    let eye_height = if ctx.source.player_id().is_some() {
        1.62
    } else {
        0.0
    };
    let p = ctx.source.position;
    let eye = [p[0], p[1] + eye_height, p[2]];
    let look = [
        -yaw.sin() * pitch.cos() * length,
        -pitch.sin() * length,
        yaw.cos() * pitch.cos() * length,
    ];
    (eye, look)
}

async fn find(ctx: &mut Context<'_>, finder: &Finder, registry: &TestRegistry) -> Found {
    let dimension = ctx.source.dimension;
    let center = source_block(ctx);
    let mut found = Found {
        instances: Vec::new(),
        positions: Vec::new(),
    };
    match finder {
        Finder::Selection(ids) => found.instances = ids.clone(),
        Finder::Failed { only_required } => {
            if let Ok(tests) = service(ctx.shared()) {
                found.instances = tests
                    .failed()
                    .await
                    .into_iter()
                    .filter(|id| {
                        !only_required || registry.instances.get(id).is_some_and(|i| i.required)
                    })
                    .collect();
            }
        }
        Finder::Nearest => {
            let near = test_blocks_near(ctx, dimension, center, NEARBY_RADIUS).await;
            if let Some(pos) = near.into_iter().min_by_key(|p| manhattan(*p, center)) {
                found.positions.push((dimension, pos));
            }
        }
        Finder::Nearby => {
            let near = test_blocks_near(ctx, dimension, center, FULL_RADIUS).await;
            found.positions = near.into_iter().map(|p| (dimension, p)).collect();
        }
        Finder::Radius(radius) => {
            let near = test_blocks_near(ctx, dimension, center, *radius).await;
            found.positions = near.into_iter().map(|p| (dimension, p)).collect();
        }
        Finder::LookedAt => {
            let (eye, look) = look_vector(ctx, f64::from(FULL_RADIUS));
            let near = test_blocks_near(ctx, dimension, center, FULL_RADIUS).await;
            let mut hits = Vec::new();
            for pos in near {
                if let Some(placed) = read_placed(ctx, registry, dimension, pos).await {
                    let (low, high) = placed.geometry.structure_box();
                    if ray_hits(eye, look, low, high) {
                        hits.push(pos);
                    }
                }
            }
            let distance =
                |p: &[i32; 3]| -> i64 { (0..3).map(|i| i64::from(p[i] - center[i]).pow(2)).sum() };
            if let Some(pos) = hits.into_iter().min_by_key(distance) {
                found.positions.push((dimension, pos));
            }
        }
    }
    found
}

fn copies<T: Clone>(list: Vec<T>, amount: usize) -> Vec<T> {
    let mut out = Vec::with_capacity(list.len() * amount);
    for _ in 0..amount {
        out.extend(list.iter().cloned());
    }
    out
}

fn structure_missing(ctx: &Context<'_>, world: &WorldAccess, structure: &str) -> bool {
    if world.template_size(structure).is_some() {
        return false;
    }
    ctx.fail(TextComponent::translate(
        "commands.test.error.structure_not_found",
        [TextComponent::new(structure)],
    ));
    true
}

/// Tests for the test instance blocks at `positions`.
async fn specs_at(
    ctx: &mut Context<'_>,
    world: &WorldAccess,
    registry: &TestRegistry,
    positions: &[(DimensionId, [i32; 3])],
    retry: RetryOptions,
) -> Vec<TestSpec> {
    let mut specs = Vec::new();
    for (dimension, pos) in positions {
        let Some(placed) = read_placed(ctx, registry, *dimension, *pos).await else {
            ctx.fail(TextComponent::translate(
                "commands.test.error.test_instance_not_found.position",
                pos.map(|v| TextComponent::new(v.to_string())),
            ));
            continue;
        };
        let name = placed.data.test.clone().unwrap_or_default();
        let Some(instance) = placed
            .data
            .test
            .as_ref()
            .and_then(|t| registry.instances.get(t))
        else {
            ctx.fail(TextComponent::translate(
                "commands.test.error.non_existant_test",
                [test_name(placed.data.test.as_deref())],
            ));
            continue;
        };
        if structure_missing(ctx, world, &instance.structure) {
            continue;
        }
        specs.push(TestSpec {
            id: name,
            instance: instance.clone(),
            dimension: *dimension,
            extra_rotation: placed.data.rotation,
            retry,
            test_pos: Some(placed.pos),
        });
    }
    specs
}

/// Tests for registry entries, on the grid.
fn specs_of(
    ctx: &Context<'_>,
    world: &WorldAccess,
    registry: &TestRegistry,
    ids: &[String],
    retry: RetryOptions,
    rotation: Rotation,
) -> Vec<TestSpec> {
    let mut specs = Vec::new();
    for id in ids {
        let Some(instance) = registry.instances.get(id) else {
            continue;
        };
        if structure_missing(ctx, world, &instance.structure) {
            continue;
        }
        let Some(dimension) = ctx
            .shared()
            .dimensions
            .by_key(&instance.dimension)
            .map(|d| d.id)
        else {
            ctx.fail(TextComponent::new(format!(
                "Could not resolve level for dimension: {}",
                instance.dimension
            )));
            continue;
        };
        specs.push(TestSpec {
            id: id.clone(),
            instance: instance.clone(),
            dimension,
            extra_rotation: rotation,
            retry,
            test_pos: None,
        });
    }
    specs
}

fn test_name(test: Option<&str>) -> TextComponent {
    match test {
        Some(test) => TextComponent::new(test),
        None => TextComponent::translate("test_instance_block.invalid_test", []),
    }
}

/// Where new tests go: three blocks south of the source, on the surface
/// of `dimension`.
async fn test_position_around(
    ctx: &Context<'_>,
    world: &mut WorldAccess,
    dimension: DimensionId,
) -> [i32; 3] {
    let [x, _, z] = source_block(ctx);
    let y = world.surface_y(dimension, x, z).await;
    [x, y, z + TEST_POS_Z_OFFSET]
}

async fn start_run(
    ctx: &mut Context<'_>,
    world: &mut WorldAccess,
    specs: Vec<TestSpec>,
    batches: Option<Vec<runner::Batch>>,
    tests_per_row: i32,
    verify: bool,
) -> Result<i32, CommandError> {
    let mut origins = HashMap::new();
    let dimensions: Vec<DimensionId> = match &batches {
        Some(batches) => batches.iter().map(|b| b.dimension).collect(),
        None => specs.iter().map(|s| s.dimension).collect(),
    };
    for dimension in dimensions {
        if let std::collections::hash_map::Entry::Vacant(entry) = origins.entry(dimension) {
            entry.insert(test_position_around(ctx, world, dimension).await);
        }
    }
    let max_per_batch = if verify {
        VERIFY_BATCH
    } else {
        runner::MAX_TESTS_PER_BATCH
    };
    let plan = RunPlan {
        batches: batches.unwrap_or_else(|| into_batches(specs, max_per_batch)),
        origins,
        tests_per_row,
        clear_on_batch: verify,
        grid_for_existing: verify,
        halt_on_error: verify,
        clear_between_batches: verify,
        max_per_batch,
        reporter: Reporter::Source {
            target: ctx.source.feedback_target(),
            name: ctx.source_display_name(),
        },
        junit: None,
        done: None,
    };
    service(ctx.shared())?.start(plan).await;
    Ok(1)
}

async fn run(
    ctx: &mut Context<'_>,
    finder: Finder,
    retry: RetryOptions,
    rotation_steps: i32,
    tests_per_row: i32,
    amount: usize,
) -> Result<i32, CommandError> {
    let tests = service(ctx.shared())?;
    tests.stop().await;
    let contents = ctx.shared().datapacks.contents();
    let registry = TestRegistry::current(&contents);
    let found = find(ctx, &finder, &registry).await;
    if !(0..=3).contains(&rotation_steps) {
        return Err(CommandError::text(TextComponent::new(format!(
            "rotationSteps must be a value from 0-3. Got value {rotation_steps}"
        ))));
    }
    let mut world = WorldAccess::new(ctx.shared());
    let mut specs = specs_at(
        ctx,
        &world,
        &registry,
        &copies(found.positions, amount),
        retry,
    )
    .await;
    specs.extend(specs_of(
        ctx,
        &world,
        &registry,
        &copies(found.instances, amount),
        retry,
        Rotation::from_turns(i64::from(rotation_steps)),
    ));
    if specs.is_empty() {
        ctx.success(
            TextComponent::translate("commands.test.no_tests", []),
            false,
        )
        .await;
        world.finish().await;
        return Ok(0);
    }
    ctx.success(
        TextComponent::translate(
            "commands.test.run.running",
            [TextComponent::new(specs.len().to_string())],
        ),
        false,
    )
    .await;
    let result = start_run(ctx, &mut world, specs, None, tests_per_row, false).await;
    world.finish().await;
    result
}

async fn verify(ctx: &mut Context<'_>, ids: Vec<String>) -> Result<i32, CommandError> {
    let tests = service(ctx.shared())?;
    tests.stop().await;
    let contents = ctx.shared().datapacks.contents();
    let registry = TestRegistry::current(&contents);
    let mut world = WorldAccess::new(ctx.shared());
    let specs = specs_of(
        ctx,
        &world,
        &registry,
        &ids,
        RetryOptions::NONE,
        Rotation::None,
    );
    let mut batches = Vec::new();
    for spec in specs {
        for rotation in Rotation::ALL {
            let mut copy = spec.clone();
            copy.extra_rotation = rotation;
            copy.retry = RetryOptions {
                tries: 1,
                halt_on_failure: true,
            };
            batches.push(runner::Batch {
                index: rotation.turns() as usize,
                environment: spec.instance.environment.clone(),
                dimension: spec.dimension,
                tests: vec![copy; VERIFY_COPIES],
            });
        }
    }
    let result = start_run(
        ctx,
        &mut world,
        Vec::new(),
        Some(batches),
        VERIFY_GRID_AXIS,
        true,
    )
    .await;
    world.finish().await;
    result
}

async fn locate(ctx: &mut Context<'_>, ids: Vec<String>) -> Result<i32, CommandError> {
    ctx.source
        .feedback_target()
        .send(TextComponent::translate("commands.test.locate.started", []));
    let contents = ctx.shared().datapacks.contents();
    let registry = TestRegistry::current(&contents);
    let center = source_block(ctx);
    let here = ctx.source.dimension;
    let dimensions: Vec<(DimensionId, String)> = ctx
        .shared()
        .dimensions
        .iter()
        .map(|d| (d.id, d.key.clone()))
        .collect();
    let mut count = 0;
    for (dimension, key) in dimensions {
        for pos in test_blocks_near(ctx, dimension, center, FULL_RADIUS).await {
            let Some(placed) = read_placed(ctx, &registry, dimension, pos).await else {
                continue;
            };
            if !placed.data.test.as_ref().is_some_and(|t| ids.contains(t)) {
                continue;
            }
            let facing = match placed.geometry.rotation {
                Rotation::None => [0, -1, 180],
                Rotation::Clockwise90 => [1, 0, -90],
                Rotation::Clockwise180 => [0, 1, 0],
                Rotation::Counterclockwise90 => [-1, 0, 90],
            };
            let tp = [pos[0] + facing[0] * 3, pos[1], pos[2] + facing[1] * 3];
            let command = format!(
                "/execute in {key} run tp @s {} {} {} {} 0",
                tp[0], tp[1], tp[2], facing[2]
            );
            let distance = if dimension == here {
                let dx = f64::from(center[0] - pos[0]);
                let dz = f64::from(center[2] - pos[2]);
                ((dx * dx + dz * dz).sqrt().floor() as i64).to_string()
            } else {
                "N/A".to_owned()
            };
            let path = key.split_once(':').map_or(key.as_str(), |(_, p)| p);
            let coordinates = TextComponent::new("[")
                .append(
                    TextComponent::translate(
                        "chat.coordinates",
                        pos.map(|v| TextComponent::new(v.to_string())),
                    )
                    .append(TextComponent::new(format!(" ->[{path}]"))),
                )
                .append(TextComponent::new("]"))
                .color("green")
                .click(ClickEvent::SuggestCommand(command))
                .hover_text(TextComponent::translate("chat.coordinates.tooltip", []));
            ctx.success(
                TextComponent::translate(
                    "commands.test.locate.found",
                    [coordinates, TextComponent::new(distance)],
                ),
                false,
            )
            .await;
            count += 1;
        }
    }
    if count == 0 {
        return Err(CommandError::simple(
            "commands.test.error.no_test_instances",
        ));
    }
    ctx.success(
        TextComponent::translate(
            "commands.test.locate.done",
            [TextComponent::new(count.to_string())],
        ),
        true,
    )
    .await;
    Ok(count)
}

async fn reset(ctx: &mut Context<'_>, finder: Finder) -> Result<i32, CommandError> {
    service(ctx.shared())?.stop().await;
    let contents = ctx.shared().datapacks.contents();
    let registry = TestRegistry::current(&contents);
    let found = find(ctx, &finder, &registry).await;
    let mut world = WorldAccess::new(ctx.shared());
    let specs = specs_at(ctx, &world, &registry, &found.positions, RetryOptions::NONE).await;
    let count = specs.len() as i32;
    for spec in specs {
        let Some(pos) = spec.test_pos else { continue };
        let Some(placed) = read_placed(ctx, &registry, spec.dimension, pos).await else {
            continue;
        };
        runner::remove_barriers(
            &mut world,
            spec.dimension,
            &placed.geometry,
            !spec.instance.sky_access,
        )
        .await;
        let placed_ok = runner::place_test_structure(
            &mut world,
            spec.dimension,
            &placed.geometry,
            &spec.instance.structure,
        )
        .await;
        if placed_ok {
            ctx.source.feedback_target().send(
                TextComponent::translate(
                    "test_instance_block.reset_success",
                    [TextComponent::new(spec.id.clone())],
                )
                .color("green"),
            );
        }
        world
            .set_instance_block(
                spec.dimension,
                pos,
                &test_instance_nbt(
                    Some(&spec.id),
                    placed.data.size,
                    placed.data.rotation,
                    "cleared",
                    None,
                ),
            )
            .await;
    }
    world.finish().await;
    if count == 0 {
        return Err(CommandError::simple("commands.test.clear.error.no_tests"));
    }
    ctx.success(
        TextComponent::translate(
            "commands.test.reset.success",
            [TextComponent::new(count.to_string())],
        ),
        true,
    )
    .await;
    Ok(count)
}

async fn clear(ctx: &mut Context<'_>, finder: Finder) -> Result<i32, CommandError> {
    service(ctx.shared())?.stop().await;
    let contents = ctx.shared().datapacks.contents();
    let registry = TestRegistry::current(&contents);
    let found = find(ctx, &finder, &registry).await;
    let mut world = WorldAccess::new(ctx.shared());
    let mut count = 0;
    for (dimension, pos) in found.positions {
        let Some(placed) = read_placed(ctx, &registry, dimension, pos).await else {
            continue;
        };
        let sky = placed
            .data
            .test
            .as_ref()
            .and_then(|t| registry.instances.get(t))
            .is_some_and(|i| i.sky_access);
        world
            .clear_space(dimension, placed.geometry.test_box())
            .await;
        runner::remove_barriers(&mut world, dimension, &placed.geometry, !sky).await;
        world
            .set_all(dimension, &[(pos, Block::Air.default_state())])
            .await;
        count += 1;
    }
    world.finish().await;
    if count == 0 {
        return Err(CommandError::simple("commands.test.clear.error.no_tests"));
    }
    ctx.success(
        TextComponent::translate(
            "commands.test.clear.success",
            [TextComponent::new(count.to_string())],
        ),
        true,
    )
    .await;
    Ok(count)
}

async fn create(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let Some(Value::Identifier(id)) = ctx.arg("id") else {
        return Err(CommandError::simple("command.unknown.argument"));
    };
    let id = if id.contains(':') {
        id.clone()
    } else {
        format!("minecraft:{id}")
    };
    let width = int_arg(ctx, "width").unwrap_or(5);
    let size = [
        width,
        int_arg(ctx, "height").unwrap_or(width),
        int_arg(ctx, "depth").unwrap_or(width),
    ];
    if size.iter().any(|v| *v > MAX_CREATE_SIZE) {
        return Err(CommandError::dynamic(
            "commands.test.error.too_large",
            [TextComponent::new(MAX_CREATE_SIZE.to_string())],
        ));
    }
    let dimension = ctx.source.dimension;
    let mut world = WorldAccess::new(ctx.shared());
    let pos = test_position_around(ctx, &mut world, dimension).await;
    let geometry = Geometry {
        block: pos,
        size,
        rotation: Rotation::None,
        padding: 0,
    };
    world.clear_space(dimension, geometry.structure_box()).await;
    world
        .set_instance_block(
            dimension,
            pos,
            &test_instance_nbt(Some(&id), size, Rotation::None, "cleared", None),
        )
        .await;
    let low = geometry.structure_pos();
    let bedrock = Block::Bedrock.default_state();
    let mut floor = Vec::new();
    for z in low[2]..low[2] + size[2] {
        for x in low[0]..low[0] + size[0] {
            floor.push(([x, low[1], z], bedrock));
        }
    }
    world.set_all(dimension, &floor).await;
    world.finish().await;
    ctx.success(
        TextComponent::translate("commands.test.create.success", [TextComponent::new(id)]),
        true,
    )
    .await;
    Ok(1)
}

async fn show_pos(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    if ctx.source.player_id().is_none() {
        return Err(CommandError::simple("permissions.requires.player"));
    }
    let var = match ctx.arg("var") {
        Some(Value::Str(var)) => var.clone(),
        _ => "pos".to_owned(),
    };
    let dimension = ctx.source.dimension;
    let (eye, look) = look_vector(ctx, 10.0);
    let mut samples: Vec<[i32; 3]> = Vec::new();
    for step in 0..=200 {
        let t = f64::from(step) / 200.0;
        let p = [0, 1, 2].map(|i| (eye[i] + look[i] * t).floor() as i32);
        if samples.last() != Some(&p) {
            samples.push(p);
        }
    }
    let states = crate::world::read_blocks(ctx, dimension, &samples, false)
        .await
        .unwrap_or_default();
    let target = samples
        .iter()
        .zip(states.iter().chain(std::iter::repeat(&None)))
        .find(|(_, s)| s.is_some_and(|s| !runner::is_air(s)))
        .map(|(p, _)| *p)
        .unwrap_or_else(|| [0, 1, 2].map(|i| (eye[i] + look[i]).floor() as i32));
    let contents = ctx.shared().datapacks.contents();
    let registry = TestRegistry::current(&contents);
    let mut containing = None;
    for radius in [NEARBY_RADIUS, FULL_RADIUS] {
        for pos in test_blocks_near(ctx, dimension, target, radius).await {
            if let Some(placed) = read_placed(ctx, &registry, dimension, pos).await
                && placed.geometry.contains(target)
            {
                containing = Some(placed);
                break;
            }
        }
        if containing.is_some() {
            break;
        }
    }
    let Some(placed) = containing else {
        return Err(CommandError::dynamic(
            "commands.test.error.no_test_containing_pos",
            target.map(|v| TextComponent::new(v.to_string())),
        ));
    };
    let origin = placed.geometry.structure_pos();
    let relative = [0, 1, 2].map(|i| target[i] - origin[i]);
    let description = format!("{}, {}, {}", relative[0], relative[1], relative[2]);
    let coords = TextComponent::translate(
        "commands.test.coordinates",
        relative.map(|v| TextComponent::new(v.to_string())),
    )
    .bold(true)
    .color("green")
    .hover_text(TextComponent::translate(
        "commands.test.coordinates.copy",
        [],
    ))
    .click(ClickEvent::CopyToClipboard(format!(
        "final BlockPos {var} = new BlockPos({description});"
    )));
    ctx.success(
        TextComponent::translate(
            "commands.test.relative_position",
            [test_name(placed.data.test.as_deref()), coords],
        ),
        false,
    )
    .await;
    Ok(1)
}

fn retry_of(ctx: &Context<'_>) -> RetryOptions {
    match (int_arg(ctx, "numberOfTimes"), bool_arg(ctx, "untilFailed")) {
        (Some(tries), until) => RetryOptions {
            tries,
            halt_on_failure: until.unwrap_or(false),
        },
        (None, _) => RetryOptions::NONE,
    }
}

fn selection(ctx: &Context<'_>, registry: &TestRegistry) -> Vec<String> {
    match ctx.arg("tests") {
        Some(Value::Resources(patterns)) => registry.select(patterns),
        _ => Vec::new(),
    }
}

/// `/test`: runs, locates, resets and clears game tests.
pub struct TestCommand;

impl Executor for TestCommand {
    fn suggest(
        &self,
        path: &[ParsedNode],
        partial: &str,
        input: &SuggestInput,
    ) -> Option<Vec<String>> {
        if path.last().map(|node| node.node.name) != Some("tests") {
            return None;
        }
        let registry = TestRegistry::current(&input.datapacks.contents());
        let partial = partial.to_lowercase();
        Some(
            registry
                .instances
                .keys()
                .filter(|id| tuff_core::command::args::matches_resource(&partial, id))
                .map(|id| id.strip_prefix("minecraft:").unwrap_or(id).to_owned())
                .collect(),
        )
    }

    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let contents = ctx.shared().datapacks.contents();
            let registry = TestRegistry::current(&contents);
            let retry = retry_of(ctx);
            let steps = int_arg(ctx, "rotationSteps").unwrap_or(0);
            let per_row = int_arg(ctx, "testsPerRow").unwrap_or(runner::DEFAULT_TESTS_PER_ROW);
            match ctx.literals().get(1).copied() {
                Some("run") => {
                    let ids = selection(ctx, &registry);
                    run(ctx, Finder::Selection(ids), retry, steps, per_row, 1).await
                }
                Some("runmultiple") => {
                    let ids = selection(ctx, &registry);
                    let amount = int_arg(ctx, "amount").unwrap_or(1).max(0) as usize;
                    run(
                        ctx,
                        Finder::Selection(ids),
                        RetryOptions::NONE,
                        0,
                        runner::DEFAULT_TESTS_PER_ROW,
                        amount,
                    )
                    .await
                }
                Some("runthese") => {
                    run(
                        ctx,
                        Finder::Nearby,
                        retry,
                        0,
                        runner::DEFAULT_TESTS_PER_ROW,
                        1,
                    )
                    .await
                }
                Some("runclosest") => {
                    run(
                        ctx,
                        Finder::Nearest,
                        retry,
                        0,
                        runner::DEFAULT_TESTS_PER_ROW,
                        1,
                    )
                    .await
                }
                Some("runthat") => {
                    run(
                        ctx,
                        Finder::LookedAt,
                        retry,
                        0,
                        runner::DEFAULT_TESTS_PER_ROW,
                        1,
                    )
                    .await
                }
                Some("runfailed") => {
                    let only_required = bool_arg(ctx, "onlyRequiredTests").unwrap_or(false);
                    run(
                        ctx,
                        Finder::Failed { only_required },
                        retry,
                        steps,
                        per_row,
                        1,
                    )
                    .await
                }
                Some("verify") => {
                    let ids = selection(ctx, &registry);
                    verify(ctx, ids).await
                }
                Some("locate") => {
                    let ids = selection(ctx, &registry);
                    locate(ctx, ids).await
                }
                Some("resetclosest") => reset(ctx, Finder::Nearest).await,
                Some("resetthese") => reset(ctx, Finder::Nearby).await,
                Some("resetthat") => reset(ctx, Finder::LookedAt).await,
                Some("clearthat") => clear(ctx, Finder::LookedAt).await,
                Some("clearthese") => clear(ctx, Finder::Nearby).await,
                Some("clearall") => {
                    let radius = int_arg(ctx, "radius")
                        .map_or(FULL_RADIUS, |r| r.clamp(0, MAX_CLEAR_RADIUS));
                    clear(ctx, Finder::Radius(radius)).await
                }
                Some("stop") => {
                    service(ctx.shared())?.stop().await;
                    Ok(1)
                }
                Some("pos") => show_pos(ctx).await,
                Some("create") => create(ctx).await,
                _ => Err(CommandError::simple("command.unknown.command")),
            }
        })
    }
}
