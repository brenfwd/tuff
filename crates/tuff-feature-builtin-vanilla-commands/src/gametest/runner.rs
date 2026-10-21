//! The game test runner: one actor per world that places test structures
//! on a grid, runs them batch by batch (one environment and dimension per
//! batch) on the game tick, retries them and reports the results.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Instant;

use tuff_core::world::Shared;
use tuff_core::world::commands::console::{self, ConsoleRuntime};
use tuff_core::world::commands::{Actor, FeedbackTarget, feedback};
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::features::TemplateRequest;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::{Block, BlockState};

use super::registry::{
    Environment, EnvironmentRef, Rotation, TestHelper, TestInstance, TestKind, TestRegistry,
    TickCallback,
};
use super::report::JUnitReport;
use crate::Context;

const SPACE_BETWEEN_COLUMNS: i32 = 5;
const SPACE_BETWEEN_ROWS: i32 = 6;
pub const DEFAULT_TESTS_PER_ROW: i32 = 8;
pub const MAX_TESTS_PER_BATCH: usize = 50;
/// Where a test structure starts relative to its test instance block.
const STRUCTURE_OFFSET: [i32; 3] = [0, 1, 1];

/// How often a test runs: `tries` below one is unlimited, and
/// `halt_on_failure` stops at the first failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryOptions {
    pub tries: i32,
    pub halt_on_failure: bool,
}

impl RetryOptions {
    pub const NONE: Self = Self {
        tries: 1,
        halt_on_failure: true,
    };

    fn unlimited(self) -> bool {
        self.tries < 1
    }

    fn has_tries_left(self, attempts: i32, successes: i32) -> bool {
        let failures = attempts != successes;
        let more = self.unlimited() || attempts < self.tries;
        more && !(failures && self.halt_on_failure)
    }

    fn has_retries(self) -> bool {
        self.tries != 1
    }
}

/// One test to run.
#[derive(Clone)]
pub struct TestSpec {
    pub id: String,
    pub instance: Arc<TestInstance>,
    pub dimension: DimensionId,
    pub extra_rotation: Rotation,
    pub retry: RetryOptions,
    /// The test instance block, for a test run where it already is.
    pub test_pos: Option<[i32; 3]>,
}

/// Where a run's results go.
#[derive(Clone)]
pub enum Reporter {
    /// A `/test` source: batch starts and the summary.
    Source {
        target: FeedbackTarget,
        name: TextComponent,
    },
    /// The headless runner: progress and the summary in the log.
    Log,
}

/// What to run and how.
pub struct RunPlan {
    pub batches: Vec<Batch>,
    /// The north-west corner of the first test of each dimension's grid.
    pub origins: HashMap<DimensionId, [i32; 3]>,
    pub tests_per_row: i32,
    /// Clears a grid's tests from the last batch before the next one.
    pub clear_on_batch: bool,
    /// Runs tests that have a position on the grid too (verify).
    pub grid_for_existing: bool,
    pub halt_on_error: bool,
    pub clear_between_batches: bool,
    pub max_per_batch: usize,
    pub reporter: Reporter,
    pub junit: Option<std::path::PathBuf>,
    pub done: Option<tokio::sync::oneshot::Sender<Summary>>,
}

/// Tests sharing an environment and dimension.
#[derive(Clone)]
pub struct Batch {
    pub index: usize,
    pub environment: EnvironmentRef,
    pub dimension: DimensionId,
    pub tests: Vec<TestSpec>,
}

/// Splits `tests` into batches by environment and dimension, in order of
/// first appearance, at most `max` tests each.
pub fn into_batches(tests: Vec<TestSpec>, max: usize) -> Vec<Batch> {
    let mut groups: Vec<((EnvironmentRef, DimensionId), Vec<TestSpec>)> = Vec::new();
    for test in tests {
        let key = (test.instance.environment.clone(), test.dimension);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(test),
            None => groups.push((key, vec![test])),
        }
    }
    let mut batches = Vec::new();
    for ((environment, dimension), list) in groups {
        for (index, chunk) in list.chunks(max.max(1)).enumerate() {
            batches.push(Batch {
                index,
                environment: environment.clone(),
                dimension,
                tests: chunk.to_vec(),
            });
        }
    }
    batches
}

/// How a finished run went.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub total: usize,
    pub passed: usize,
    pub failed_required: Vec<String>,
    pub failed_optional: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Cleared,
    Running,
    Finished,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Self::Cleared => "cleared",
            Self::Running => "running",
            Self::Finished => "finished",
        }
    }
}

/// A test structure's geometry, from its test instance block.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub block: [i32; 3],
    pub size: [i32; 3],
    pub rotation: Rotation,
    pub padding: i32,
}

impl Geometry {
    pub fn structure_pos(&self) -> [i32; 3] {
        let p = self.padding;
        [
            self.block[0] + p + STRUCTURE_OFFSET[0],
            self.block[1] + p + STRUCTURE_OFFSET[1],
            self.block[2] + p + STRUCTURE_OFFSET[2],
        ]
    }

    fn transformed_size(&self) -> [i32; 3] {
        let [x, y, z] = self.size;
        if self.rotation.swaps_axes() {
            [z, y, x]
        } else {
            [x, y, z]
        }
    }

    /// The structure's blocks, inclusive.
    pub fn structure_box(&self) -> ([i32; 3], [i32; 3]) {
        let low = self.structure_pos();
        let size = self.transformed_size();
        let high = [0, 1, 2].map(|i| low[i] + size[i].max(1) - 1);
        (low, high)
    }

    /// The structure's blocks and its padding, inclusive.
    pub fn test_box(&self) -> ([i32; 3], [i32; 3]) {
        let (low, high) = self.structure_box();
        let p = self.padding;
        (low.map(|v| v - p), high.map(|v| v + p))
    }

    /// The corner the template is placed from, which its rotation turns
    /// about.
    pub fn start_corner(&self) -> [i32; 3] {
        let [x, y, z] = self.structure_pos();
        let [sx, _, sz] = self.size;
        match self.rotation {
            Rotation::None => [x, y, z],
            Rotation::Clockwise90 => [x + sz - 1, y, z],
            Rotation::Clockwise180 => [x + sx - 1, y, z + sz - 1],
            Rotation::Counterclockwise90 => [x, y, z + sx - 1],
        }
    }

    pub fn contains(&self, pos: [i32; 3]) -> bool {
        let (low, high) = self.structure_box();
        (0..3).all(|i| low[i] <= pos[i] && pos[i] <= high[i])
    }

    /// The walls, floor and (without sky access) ceiling one block
    /// outside the structure.
    fn boundary(&self, ceiling: bool) -> Vec<[i32; 3]> {
        let (low, high) = self.structure_box();
        let low = low.map(|v| v - 1);
        let high = high.map(|v| v + 1);
        let mut out = Vec::new();
        for y in low[1]..=high[1] {
            for z in low[2]..=high[2] {
                for x in low[0]..=high[0] {
                    let edge =
                        x == low[0] || x == high[0] || z == low[2] || z == high[2] || y == low[1];
                    if edge || (y == high[1] && ceiling) {
                        out.push([x, y, z]);
                    }
                }
            }
        }
        out
    }
}

/// The test instance block's saved data, as the game stores it.
pub fn test_instance_nbt(
    test: Option<&str>,
    size: [i32; 3],
    rotation: Rotation,
    status: &str,
    error: Option<&TextComponent>,
) -> Compound {
    let mut data = Compound::new();
    if let Some(test) = test {
        data.insert("test", test.to_owned());
    }
    data.insert("size", Tag::IntArray(size.to_vec()));
    data.insert("rotation", rotation.name().to_owned());
    data.insert("ignore_entities", false);
    data.insert("status", status.to_owned());
    if let Some(error) = error {
        data.insert("error_message", error.to_nbt());
    }
    let mut nbt = Compound::new();
    nbt.insert("data", data);
    nbt
}

/// What a test instance block holds.
#[derive(Debug, Clone)]
pub struct InstanceData {
    pub test: Option<String>,
    pub size: [i32; 3],
    pub rotation: Rotation,
}

pub fn read_instance_nbt(nbt: &Compound) -> Option<InstanceData> {
    let data = nbt.get_compound("data")?;
    let size = match data.get_int_array("size") {
        Some([x, y, z]) => [*x, *y, *z],
        _ => match data.get_list("size") {
            Some([x, y, z]) => [x.as_int()?, y.as_int()?, z.as_int()?],
            _ => [0, 0, 0],
        },
    };
    Some(InstanceData {
        test: data.get_str("test").map(str::to_owned),
        size,
        rotation: data
            .get_str("rotation")
            .and_then(Rotation::from_name)
            .unwrap_or_default(),
    })
}

/// The world as the runner touches it: a silent gamemaster source.
pub struct WorldAccess {
    shared: Arc<Shared>,
    runtime: ConsoleRuntime,
    source: tuff_core::world::commands::Source,
}

impl WorldAccess {
    pub fn new(shared: &Arc<Shared>) -> Self {
        let (feedback, _lines) = tokio::sync::mpsc::unbounded_channel();
        let runtime = ConsoleRuntime::new(shared.clone(), feedback);
        let mut source = console::source(shared, FeedbackTarget::Silent);
        source.permission = PermissionLevel::Gamemasters;
        source.silent = true;
        Self {
            shared: shared.clone(),
            runtime,
            source,
        }
    }

    pub async fn finish(self) {
        self.runtime.finish().await;
    }

    pub fn ctx(&mut self, dimension: DimensionId) -> Context<'_> {
        let mut source = self.source.clone();
        source.dimension = dimension;
        Context {
            actor: Actor::Console(&mut self.runtime),
            source,
            nodes: &[],
            rest: &[],
            input: "",
            needs_result: false,
            frame: None,
        }
    }

    /// Runs `line` at `at` in `dimension`; whether it succeeded.
    pub async fn run(&mut self, dimension: DimensionId, at: [i32; 3], line: &str) -> bool {
        let mut source = self.source.clone();
        source.dimension = dimension;
        source.position = [f64::from(at[0]), f64::from(at[1]), f64::from(at[2])];
        tuff_core::world::commands::run_counted(Actor::Console(&mut self.runtime), source, line)
            .await
            > 0
    }

    pub async fn say(&mut self, color: &str, text: &str) {
        let line = TextComponent::new(text).color(color).to_value().to_string();
        let dimension = self.shared.spawn_dimension();
        self.run(dimension, [0, 0, 0], &format!("tellraw @a {line}"))
            .await;
    }

    pub async fn set_instance_block(
        &mut self,
        dimension: DimensionId,
        pos: [i32; 3],
        nbt: &Compound,
    ) {
        let mut ctx = self.ctx(dimension);
        let state = Block::TestInstanceBlock.default_state();
        let _ = crate::world::write_blocks(
            &mut ctx,
            dimension,
            &[(pos, state)],
            Some(nbt),
            [false, false, false],
        )
        .await;
    }

    pub async fn states(
        &mut self,
        dimension: DimensionId,
        positions: &[[i32; 3]],
    ) -> Vec<Option<BlockState>> {
        let mut ctx = self.ctx(dimension);
        crate::world::read_blocks(&mut ctx, dimension, positions, true)
            .await
            .unwrap_or_else(|_| vec![None; positions.len()])
    }

    pub async fn set_all(&mut self, dimension: DimensionId, changes: &[([i32; 3], BlockState)]) {
        if changes.is_empty() {
            return;
        }
        let mut ctx = self.ctx(dimension);
        let _ =
            crate::world::write_blocks(&mut ctx, dimension, changes, None, [false, false, false])
                .await;
    }

    pub async fn block_entity(
        &mut self,
        dimension: DimensionId,
        pos: [i32; 3],
    ) -> Option<(BlockState, Option<Compound>)> {
        let mut ctx = self.ctx(dimension);
        crate::world::copy_blocks(&mut ctx, dimension, &[pos])
            .await
            .ok()?
            .into_iter()
            .next()
            .flatten()
    }

    async fn test_blocks(
        &mut self,
        dimension: DimensionId,
        positions: &[[i32; 3]],
        trigger: bool,
        reset: bool,
    ) -> Vec<bool> {
        if positions.is_empty() {
            return Vec::new();
        }
        let mut ctx = self.ctx(dimension);
        crate::world::test_blocks(&mut ctx, dimension, positions, trigger, reset)
            .await
            .map(|flags| flags.into_iter().map(|f| f.unwrap_or(false)).collect())
            .unwrap_or_else(|_| vec![false; positions.len()])
    }

    /// The template's size, `None` where no template has that name.
    pub fn template_size(&self, name: &str) -> Option<[i32; 3]> {
        self.shared.services.feature_host()?.template_size(name)
    }

    /// Air over `area` and non-players inside it gone.
    pub async fn clear_space(&mut self, dimension: DimensionId, area: ([i32; 3], [i32; 3])) {
        let (low, high) = area;
        let air = Block::Air.default_state();
        let mut changes = Vec::new();
        for y in low[1]..=high[1] {
            for z in low[2]..=high[2] {
                for x in low[0]..=high[0] {
                    changes.push(([x, y, z], air));
                }
            }
        }
        self.set_all(dimension, &changes).await;
        self.discard_entities(dimension, (low, high.map(|v| v + 1)))
            .await;
    }

    /// Removes the non-player entities inside `[low, high)`.
    pub async fn discard_entities(
        &mut self,
        dimension: DimensionId,
        (low, high): ([i32; 3], [i32; 3]),
    ) {
        let d = [0, 1, 2].map(|i| high[i] - low[i]);
        let line = format!(
            "kill @e[type=!minecraft:player,x={},y={},z={},dx={},dy={},dz={}]",
            low[0], low[1], low[2], d[0], d[1], d[2]
        );
        self.run(dimension, low, &line).await;
    }

    pub async fn force_chunks(
        &mut self,
        dimension: DimensionId,
        area: ([i32; 3], [i32; 3]),
        on: bool,
    ) {
        let (low, high) = area;
        let line = format!(
            "forceload {} {} {} {} {}",
            if on { "add" } else { "remove" },
            low[0],
            low[2],
            high[0],
            high[2]
        );
        self.run(dimension, low, &line).await;
    }

    /// The first air block above the highest non-air block of the column.
    pub async fn surface_y(&mut self, dimension: DimensionId, x: i32, z: i32) -> i32 {
        let shape = self.shared.dimension(dimension).shape;
        let positions: Vec<[i32; 3]> = (shape.min_y..shape.max_y())
            .rev()
            .map(|y| [x, y, z])
            .collect();
        let states = self.states(dimension, &positions).await;
        positions
            .iter()
            .zip(states)
            .find(|(_, state)| state.is_some_and(|s| !is_air(s)))
            .map_or(shape.min_y, |(pos, _)| pos[1] + 1)
    }
}

/// Air where the walls (and `ceiling`) around a test structure have
/// barriers.
pub async fn remove_barriers(
    world: &mut WorldAccess,
    dimension: DimensionId,
    geometry: &Geometry,
    ceiling: bool,
) {
    let boundary = geometry.boundary(ceiling);
    let states = world.states(dimension, &boundary).await;
    let air = Block::Air.default_state();
    let changes: Vec<([i32; 3], BlockState)> = boundary
        .into_iter()
        .zip(states)
        .filter(|(_, s)| s.is_some_and(|s| s.block() == Block::Barrier))
        .map(|(p, _)| (p, air))
        .collect();
    world.set_all(dimension, &changes).await;
}

/// Force-loads a test structure's chunks, clears its area and places its
/// template; false if the template could not be placed.
pub async fn place_test_structure(
    world: &mut WorldAccess,
    dimension: DimensionId,
    geometry: &Geometry,
    structure: &str,
) -> bool {
    if world.template_size(structure).is_none() {
        return false;
    }
    world
        .force_chunks(dimension, geometry.structure_box(), true)
        .await;
    world.clear_space(dimension, geometry.test_box()).await;
    let (low, high) = geometry.test_box();
    let mut corners = Vec::new();
    for cz in (low[2] >> 4) - 1..=(high[2] >> 4) + 1 {
        for cx in (low[0] >> 4) - 1..=(high[0] >> 4) + 1 {
            corners.push([cx << 4, low[1], cz << 4]);
        }
    }
    world.states(dimension, &corners).await;
    let [x, y, z] = geometry.start_corner();
    let request = TemplateRequest {
        name: structure.to_owned(),
        pos: tuff_world_model::BlockPos::new(x, y, z),
        rotation: geometry.rotation.turns(),
        mirror: 0,
        integrity: 1.0,
        seed: 0,
        strict: false,
        ignore_entities: false,
    };
    let mut ctx = world.ctx(dimension);
    match crate::place::place_template_request(&mut ctx, request).await {
        Ok(()) => true,
        Err(err) => {
            tuff_core::log::warn!(
                "Could not place test structure {structure}: {}",
                err.message.plain()
            );
            false
        }
    }
}

pub fn is_air(state: BlockState) -> bool {
    matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir)
}

/// An inclusive block box, low and high corners.
type Area = ([i32; 3], [i32; 3]);

struct Lineage {
    attempts: i32,
    successes: i32,
}

struct Info {
    spec: TestSpec,
    lineage: usize,
    pos: Option<[i32; 3]>,
    geometry: Option<Geometry>,
    placed: bool,
    tick: i32,
    started: bool,
    done: bool,
    error: Option<TextComponent>,
    timer: Option<Instant>,
    run_ms: u64,
    each_tick: Vec<TickCallback>,
    at_tick: Vec<(i32, TickCallback)>,
    block_tests: Option<BlockTests>,
    reported: bool,
}

struct BlockTests {
    accept: Vec<[i32; 3]>,
    fail: Vec<[i32; 3]>,
    log: Vec<[i32; 3]>,
}

impl Info {
    fn new(spec: TestSpec, lineage: usize) -> Self {
        let pos = spec.test_pos;
        Self {
            spec,
            lineage,
            pos,
            geometry: None,
            placed: false,
            tick: 0,
            started: false,
            done: false,
            error: None,
            timer: None,
            run_ms: 0,
            each_tick: Vec::new(),
            at_tick: Vec::new(),
            block_tests: None,
            reported: false,
        }
    }

    fn rotation(&self) -> Rotation {
        self.spec.instance.rotation.then(self.spec.extra_rotation)
    }

    fn fail(&mut self, message: TextComponent) {
        self.error = Some(TextComponent::translate(
            "test.error.tick",
            [message, TextComponent::new(self.tick.to_string())],
        ));
    }

    fn finish(&mut self) {
        if !self.done {
            self.done = true;
            if let Some(timer) = self.timer.take() {
                self.run_ms = timer.elapsed().as_millis() as u64;
            }
        }
    }

    fn passed(&self) -> bool {
        self.done && self.error.is_none()
    }

    fn failed(&self) -> bool {
        self.error.is_some()
    }

    fn pos_text(&self) -> String {
        self.pos
            .map(|[x, y, z]| format!("{x}, {y}, {z}"))
            .unwrap_or_else(|| "?".to_owned())
    }

    fn message(&self) -> String {
        self.error
            .as_ref()
            .map(TextComponent::plain)
            .unwrap_or_default()
    }
}

struct Grid {
    first: [i32; 3],
    next: [i32; 3],
    row_low: [i32; 3],
    row_high: [i32; 3],
    row_count: i32,
    last_batch: Vec<usize>,
}

impl Grid {
    fn new(first: [i32; 3]) -> Self {
        Self {
            first,
            next: first,
            row_low: first,
            row_high: first.map(|v| v + 1),
            row_count: 0,
            last_batch: Vec::new(),
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.first);
    }
}

/// One run of `/test` or the headless runner.
pub struct Run {
    infos: Vec<Info>,
    lineages: Vec<Lineage>,
    batches: Vec<Batch>,
    /// The infos of each batch, by index into `infos`.
    batch_infos: Vec<Vec<usize>>,
    batch: usize,
    /// Tests that finished this tick and run again.
    pending_rerun: Vec<usize>,
    /// The infos of the running batch.
    current: Vec<usize>,
    environment: Vec<Teardown>,
    rerun: Vec<usize>,
    stopped: bool,
    grids: HashMap<DimensionId, Grid>,
    forced: Vec<(DimensionId, Area)>,
    plan_origins: HashMap<DimensionId, [i32; 3]>,
    tests_per_row: i32,
    clear_on_batch: bool,
    grid_for_existing: bool,
    halt_on_error: bool,
    clear_between_batches: bool,
    max_per_batch: usize,
    reporter: Reporter,
    junit: Option<JUnitReport>,
    done: Option<tokio::sync::oneshot::Sender<Summary>>,
    summarized: bool,
    pub failed_tests: BTreeSet<String>,
    started: Instant,
}

enum Teardown {
    Command(DimensionId, String),
}

impl Run {
    pub fn new(plan: RunPlan) -> Self {
        let mut run = Self {
            infos: Vec::new(),
            lineages: Vec::new(),
            batches: Vec::new(),
            batch_infos: Vec::new(),
            batch: 0,
            pending_rerun: Vec::new(),
            current: Vec::new(),
            environment: Vec::new(),
            rerun: Vec::new(),
            stopped: true,
            grids: HashMap::new(),
            forced: Vec::new(),
            plan_origins: plan.origins,
            tests_per_row: plan.tests_per_row,
            clear_on_batch: plan.clear_on_batch,
            grid_for_existing: plan.grid_for_existing,
            halt_on_error: plan.halt_on_error,
            clear_between_batches: plan.clear_between_batches,
            max_per_batch: plan.max_per_batch,
            reporter: plan.reporter,
            junit: plan.junit.map(JUnitReport::new),
            done: plan.done,
            summarized: false,
            failed_tests: BTreeSet::new(),
            started: Instant::now(),
        };
        let mut batches = Vec::new();
        for batch in plan.batches {
            let mut indices = Vec::new();
            for spec in &batch.tests {
                indices.push(run.add(spec.clone(), None));
            }
            batches.push((batch, indices));
        }
        run.batches = batches.iter().map(|(b, _)| b.clone()).collect();
        run.batch_infos = batches.into_iter().map(|(_, i)| i).collect();
        run
    }

    fn add(&mut self, spec: TestSpec, lineage: Option<usize>) -> usize {
        let lineage = lineage.unwrap_or_else(|| {
            self.lineages.push(Lineage {
                attempts: 0,
                successes: 0,
            });
            self.lineages.len() - 1
        });
        self.infos.push(Info::new(spec, lineage));
        self.infos.len() - 1
    }

    pub fn total(&self) -> usize {
        self.infos.len()
    }

    pub fn is_done(&self) -> bool {
        self.infos.iter().all(|i| i.done)
    }

    pub fn summary(&self) -> Summary {
        let mut summary = Summary {
            total: self.infos.len(),
            ..Summary::default()
        };
        for info in &self.infos {
            if info.passed() {
                summary.passed += 1;
            } else if info.failed() {
                let line = if info.rotation() == info.spec.instance.rotation
                    || info.spec.extra_rotation == Rotation::None
                {
                    format!("{}: {}", info.spec.id, info.message())
                } else {
                    format!(
                        "{} with rotation {}: {}",
                        info.spec.id,
                        info.rotation().name(),
                        info.message()
                    )
                };
                if info.spec.instance.required {
                    summary.failed_required.push(line);
                } else {
                    summary.failed_optional.push(line);
                }
            }
        }
        summary
    }

    /// `[+X_ ]`: one mark per test, as the game logs progress.
    pub fn progress_bar(&self) -> String {
        let mut bar = String::from("[");
        for info in &self.infos {
            bar.push(if !info.started {
                ' '
            } else if info.passed() {
                '+'
            } else if info.failed() {
                if info.spec.instance.required {
                    'X'
                } else {
                    'x'
                }
            } else {
                '_'
            });
        }
        bar.push(']');
        bar
    }

    pub async fn start(&mut self, world: &mut WorldAccess, registry: &TestRegistry) {
        self.stopped = false;
        self.run_batch(world, registry, 0).await;
    }

    /// Stops ticking and tears the environment down.
    pub async fn stop(&mut self, world: &mut WorldAccess) {
        self.stopped = true;
        self.current.clear();
        self.end_environment(world).await;
        self.unforce(world).await;
    }

    async fn unforce(&mut self, world: &mut WorldAccess) {
        for (dimension, area) in std::mem::take(&mut self.forced) {
            world.force_chunks(dimension, area, false).await;
        }
    }

    async fn end_environment(&mut self, world: &mut WorldAccess) {
        for teardown in std::mem::take(&mut self.environment) {
            match teardown {
                Teardown::Command(dimension, line) => {
                    world.run(dimension, [0, 0, 0], &line).await;
                }
            }
        }
    }

    async fn activate(
        &mut self,
        world: &mut WorldAccess,
        registry: &TestRegistry,
        environment: &EnvironmentRef,
        dimension: DimensionId,
        depth: usize,
    ) {
        let Some(environment) = registry.environment(environment) else {
            tuff_core::log::error!("Missing test environment {}", environment.display_name());
            return;
        };
        if depth > 16 {
            return;
        }
        let shared = world.shared.clone();
        let mut setup: Vec<String> = Vec::new();
        match environment {
            Environment::AllOf(parts) => {
                for part in &parts {
                    Box::pin(self.activate(world, registry, part, dimension, depth + 1)).await;
                }
            }
            Environment::ClockTime { clock, time } => {
                if let Some(id) = tuff_core::world::clock::clock_id(&clock) {
                    let previous = shared.clocks.borrow().get(id).total_ticks;
                    setup.push(format!("time of {clock} set {time}"));
                    self.environment.push(Teardown::Command(
                        dimension,
                        format!("time of {clock} set {previous}"),
                    ));
                }
            }
            Environment::Difficulty(difficulty) => {
                let previous = shared.difficulty().name();
                setup.push(format!("difficulty {difficulty}"));
                self.environment.push(Teardown::Command(
                    dimension,
                    format!("difficulty {previous}"),
                ));
            }
            Environment::Functions {
                setup: on,
                teardown,
            } => {
                if let Some(function) = on {
                    if shared.datapacks.contents().function(&function).is_some() {
                        setup.push(format!("function {function}"));
                    } else {
                        tuff_core::log::error!(
                            "Test Batch failed for non-existent function {function}"
                        );
                    }
                }
                if let Some(function) = teardown {
                    self.environment
                        .push(Teardown::Command(dimension, format!("function {function}")));
                }
            }
            Environment::GameRules(rules) => {
                let current = shared.game_rules.borrow().clone();
                for (name, value) in rules {
                    if let Some(previous) = current.get(&name) {
                        self.environment.push(Teardown::Command(
                            dimension,
                            format!("gamerule {name} {}", previous.serialize()),
                        ));
                    }
                    setup.push(format!("gamerule {name} {value}"));
                }
            }
            Environment::Timelines(timelines) => {
                tuff_core::log::warn!(
                    "Test environment timeline attributes are not applied: {}",
                    timelines.join(", ")
                );
            }
            Environment::Weather(kind) => {
                let weather = *shared.weather.borrow();
                let previous = if weather.thundering {
                    "thunder"
                } else if weather.raining {
                    "rain"
                } else {
                    "clear"
                };
                setup.push(format!("weather {kind} 100000"));
                self.environment
                    .push(Teardown::Command(dimension, format!("weather {previous}")));
            }
        }
        for line in setup {
            world.run(dimension, [0, 0, 0], &line).await;
        }
    }

    async fn run_batch(&mut self, world: &mut WorldAccess, registry: &TestRegistry, index: usize) {
        if index >= self.batches.len() {
            self.end_environment(world).await;
            self.run_scheduled_reruns(world, registry).await;
            return;
        }
        if index > 0 && self.clear_between_batches {
            let last: Vec<usize> = self.batch_infos[index - 1].clone();
            for i in last {
                let dimension = self.infos[i].spec.dimension;
                if let Some(geometry) = self.infos[i].geometry {
                    world.clear_space(dimension, geometry.test_box()).await;
                    world
                        .set_all(dimension, &[(geometry.block, Block::Air.default_state())])
                        .await;
                }
            }
        }
        self.batch = index;
        let batch = self.batches[index].clone();
        if self.clear_on_batch {
            for grid in self.grids.values_mut() {
                let last = std::mem::take(&mut grid.last_batch);
                for i in last {
                    if let Some(geometry) = self.infos[i].geometry {
                        world
                            .clear_space(self.infos[i].spec.dimension, geometry.test_box())
                            .await;
                    }
                }
                grid.reset();
            }
        }
        let mut running = Vec::new();
        for i in self.batch_infos[index].clone() {
            if self.spawn(world, registry, i).await {
                running.push(i);
            }
        }
        tuff_core::log::info!(
            "Running test environment '{}' batch {} ({} tests)...",
            batch.environment.display_name(),
            batch.index,
            running.len()
        );
        self.end_environment(world).await;
        self.activate(world, registry, &batch.environment, batch.dimension, 0)
            .await;
        if let Reporter::Source { target, name } = &self.reporter {
            feedback::success(
                &world.shared,
                target,
                name.clone(),
                TextComponent::translate(
                    "commands.test.batch.starting",
                    [
                        TextComponent::new(batch.environment.display_name()),
                        TextComponent::new(batch.index.to_string()),
                    ],
                ),
                true,
            )
            .await;
        }
        self.current = running;
        if self.current.is_empty() {
            Box::pin(self.batch_finished(world, registry)).await;
        }
    }

    async fn batch_finished(&mut self, world: &mut WorldAccess, registry: &TestRegistry) {
        self.unforce(world).await;
        let next = self.batch + 1;
        Box::pin(self.run_batch(world, registry, next)).await;
    }

    async fn run_scheduled_reruns(&mut self, world: &mut WorldAccess, registry: &TestRegistry) {
        if self.rerun.is_empty() {
            self.batches.clear();
            self.batch_infos.clear();
            self.stopped = true;
            return;
        }
        let ids: Vec<String> = self
            .rerun
            .iter()
            .map(|i| self.infos[*i].spec.id.clone())
            .collect();
        tuff_core::log::info!("Starting re-run of tests: {}", ids.join(", "));
        let rerun = std::mem::take(&mut self.rerun);
        let specs: Vec<(usize, TestSpec)> = rerun
            .iter()
            .map(|i| (*i, self.infos[*i].spec.clone()))
            .collect();
        let mut groups: Vec<Batch> = into_batches(
            specs.iter().map(|(_, s)| s.clone()).collect(),
            self.max_per_batch,
        );
        let mut cursor = 0;
        self.batch_infos = Vec::new();
        for group in &mut groups {
            let count = group.tests.len();
            self.batch_infos.push(
                specs[cursor..cursor + count]
                    .iter()
                    .map(|(i, _)| *i)
                    .collect(),
            );
            cursor += count;
        }
        self.batches = groups;
        self.stopped = false;
        Box::pin(self.run_batch(world, registry, 0)).await;
    }

    /// Places a test: on the grid when it has no position yet, else where
    /// it is.
    async fn spawn(&mut self, world: &mut WorldAccess, registry: &TestRegistry, i: usize) -> bool {
        let dimension = self.infos[i].spec.dimension;
        let on_grid = self.infos[i].pos.is_none() || self.grid_for_existing;
        if !on_grid {
            return self.prepare(world, registry, i).await;
        }
        let origin = self
            .plan_origins
            .get(&dimension)
            .copied()
            .unwrap_or([0, 0, 0]);
        let grid = self
            .grids
            .entry(dimension)
            .or_insert_with(|| Grid::new(origin));
        self.infos[i].pos = Some(grid.next);
        if !self.prepare(world, registry, i).await {
            return false;
        }
        let Some(geometry) = self.infos[i].geometry else {
            return false;
        };
        let grid = self.grids.get_mut(&dimension).expect("grid made above");
        let (low, high) = geometry.test_box();
        let high = high.map(|v| v + 1);
        for axis in 0..3 {
            grid.row_low[axis] = grid.row_low[axis].min(low[axis]);
            grid.row_high[axis] = grid.row_high[axis].max(high[axis]);
        }
        grid.next[0] += high[0] - low[0] + SPACE_BETWEEN_COLUMNS;
        grid.row_count += 1;
        if grid.row_count >= self.tests_per_row {
            grid.row_count = 0;
            grid.next[2] += grid.row_high[2] - grid.row_low[2] + SPACE_BETWEEN_ROWS;
            grid.next[0] = grid.first[0];
            grid.row_low = grid.next;
            grid.row_high = grid.next.map(|v| v + 1);
        }
        grid.last_batch.push(i);
        true
    }

    /// Puts the test instance block down, places the structure, walls it
    /// in and schedules the start.
    async fn prepare(
        &mut self,
        world: &mut WorldAccess,
        _registry: &TestRegistry,
        i: usize,
    ) -> bool {
        let Some(pos) = self.infos[i].pos else {
            return false;
        };
        let spec = self.infos[i].spec.clone();
        let dimension = spec.dimension;
        let size = world.template_size(&spec.instance.structure);
        let rotation = self.infos[i].rotation();
        let geometry = Geometry {
            block: pos,
            size: size.unwrap_or([1, 1, 1]),
            rotation,
            padding: spec.instance.padding,
        };
        world
            .set_instance_block(
                dimension,
                pos,
                &test_instance_nbt(
                    Some(&spec.id),
                    geometry.size,
                    spec.extra_rotation,
                    Status::Cleared.name(),
                    None,
                ),
            )
            .await;
        self.infos[i].geometry = Some(geometry);
        if !self.infos[i].placed {
            let (low, high) = geometry.structure_box();
            let placed = if size.is_some() {
                self.forced.push((dimension, (low, high)));
                place_test_structure(world, dimension, &geometry, &spec.instance.structure).await
            } else {
                false
            };
            if !placed {
                self.infos[i].fail(TextComponent::translate(
                    "test.error.structure.failure",
                    [TextComponent::new(spec.id.clone())],
                ));
            }
            self.infos[i].placed = true;
            let barrier = Block::Barrier.default_state();
            let walls: Vec<([i32; 3], BlockState)> = geometry
                .boundary(!spec.instance.sky_access)
                .into_iter()
                .filter(|p| *p != pos)
                .map(|p| (p, barrier))
                .collect();
            world.set_all(dimension, &walls).await;
            self.lineages[self.infos[i].lineage].attempts += 1;
        }
        self.infos[i].tick = -(spec.instance.setup_ticks + 2);
        true
    }

    async fn remove_barriers(&mut self, world: &mut WorldAccess, i: usize) {
        let Some(geometry) = self.infos[i].geometry else {
            return;
        };
        let info = &self.infos[i];
        remove_barriers(
            world,
            info.spec.dimension,
            &geometry,
            !info.spec.instance.sky_access,
        )
        .await;
    }

    async fn set_status(
        &mut self,
        world: &mut WorldAccess,
        i: usize,
        status: Status,
        error: Option<TextComponent>,
    ) {
        let info = &self.infos[i];
        let (Some(pos), Some(geometry)) = (info.pos, info.geometry) else {
            return;
        };
        let nbt = test_instance_nbt(
            Some(&info.spec.id),
            geometry.size,
            info.spec.extra_rotation,
            status.name(),
            error.as_ref(),
        );
        world
            .set_instance_block(info.spec.dimension, pos, &nbt)
            .await;
    }

    /// One game tick of every running test.
    pub async fn tick(&mut self, world: &mut WorldAccess, registry: &TestRegistry) {
        if self.stopped && self.current.is_empty() {
            return;
        }
        for i in self.current.clone() {
            if self.stopped {
                break;
            }
            self.tick_info(world, i).await;
            if self.infos[i].done && !self.infos[i].reported {
                self.infos[i].reported = true;
                if self.infos[i].error.is_some() {
                    self.on_failed(world, registry, i).await;
                } else {
                    self.on_passed(world, registry, i).await;
                }
            }
        }
        self.current.retain(|i| !self.infos[*i].done);
    }

    async fn tick_info(&mut self, world: &mut WorldAccess, i: usize) {
        if self.infos[i].done {
            return;
        }
        if self.infos[i].error.is_some() {
            self.infos[i].finish();
            return;
        }
        self.infos[i].tick += 1;
        if self.infos[i].tick < 0 {
            return;
        }
        if !self.infos[i].started {
            self.start_test(world, i).await;
        }
        let tick = self.infos[i].tick;
        let due: Vec<TickCallback> = {
            let info = &mut self.infos[i];
            let mut due = Vec::new();
            let mut keep = Vec::new();
            for (at, callback) in info.at_tick.drain(..) {
                if i64::from(at) <= i64::from(tick) {
                    due.push(callback);
                } else {
                    keep.push((at, callback));
                }
            }
            info.at_tick = keep;
            due
        };
        for callback in due {
            self.call(world, i, CallbackKind::Once(callback)).await;
        }
        if self.infos[i].tick > self.infos[i].spec.instance.max_ticks {
            if self.infos[i].error.is_none() && !self.infos[i].done {
                let max = self.infos[i].spec.instance.max_ticks;
                self.infos[i].error = Some(TextComponent::translate(
                    "test.error.timeout.no_result",
                    [TextComponent::new(max.to_string())],
                ));
            }
            return;
        }
        if self.infos[i].block_tests.is_some() {
            self.check_test_blocks(world, i).await;
        }
        let callbacks = std::mem::take(&mut self.infos[i].each_tick);
        for callback in callbacks {
            if self.infos[i].done || self.infos[i].error.is_some() {
                break;
            }
            self.call(world, i, CallbackKind::Each(callback)).await;
        }
    }

    async fn call(&mut self, world: &mut WorldAccess, i: usize, callback: CallbackKind) {
        let origin = self.infos[i]
            .geometry
            .map(|g| g.start_corner())
            .unwrap_or_default();
        let mut helper = TestHelper::new(self.infos[i].tick, origin);
        let keep = match callback {
            CallbackKind::Once(mut f) => {
                f(&mut helper);
                None
            }
            CallbackKind::Each(mut f) => {
                f(&mut helper);
                Some(f)
            }
            CallbackKind::Start(mut f) => {
                f(&mut helper);
                None
            }
        };
        if let Some(f) = keep {
            self.infos[i].each_tick.push(f);
        }
        self.absorb(world, i, helper).await;
    }

    async fn absorb(&mut self, world: &mut WorldAccess, i: usize, mut helper: TestHelper) {
        let dimension = self.infos[i].spec.dimension;
        for line in std::mem::take(&mut helper.commands) {
            world.run(dimension, helper.origin, &line).await;
        }
        self.infos[i].each_tick.append(&mut helper.each_tick);
        self.infos[i].at_tick.append(&mut helper.at_tick);
        match helper.outcome {
            Some(Ok(())) => self.succeed(world, i).await,
            Some(Err(message)) => self.infos[i].fail(TextComponent::new(message)),
            None => {}
        }
    }

    async fn succeed(&mut self, world: &mut WorldAccess, i: usize) {
        if self.infos[i].error.is_some() {
            return;
        }
        self.infos[i].finish();
        if let Some(geometry) = self.infos[i].geometry {
            let (low, high) = geometry.structure_box();
            world
                .discard_entities(
                    self.infos[i].spec.dimension,
                    (low.map(|v| v - 1), high.map(|v| v + 2)),
                )
                .await;
        }
    }

    pub fn logs_progress(&self) -> bool {
        matches!(self.reporter, Reporter::Log)
    }

    /// Whether this run has nothing left to do.
    pub fn finished(&self) -> bool {
        self.summarized || (self.stopped && self.current.is_empty() && self.rerun.is_empty())
    }
}

enum CallbackKind {
    Once(TickCallback),
    Each(TickCallback),
    Start(TickCallback),
}
mod lifecycle;
