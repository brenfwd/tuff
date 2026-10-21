//! The persistent tile cache in the world's extra files, on both storage
//! backends that have them: tiles survive a restart and are checked
//! against the world, damaged entries are ignored, a held save gate or a
//! refused write breaks nothing, and the index keeps the last uses the
//! size budget evicts by.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

use tuff_core::config::Value;
use tuff_core::world::save_gate::hold_saves;
use tuff_feature_builtin_world_map::WorldMapFeature;
use tuff_feature_builtin_world_map::disk::{self, DiskKey, Persisted};
use tuff_feature_builtin_world_map::png;
use tuff_feature_builtin_world_map::service::{DISK_KEY, MapHandle, MapService, TileReply};
use tuff_feature_builtin_world_map::tile::{SIDE, TileKey};
use tuff_storage::backend::{Documents, RegionStore, WorldLock};
use tuff_storage::sqlite::SqliteBackend;
use tuff_test_support::*;
use tuff_world_model::pos::FileRegionPos;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Vanilla,
    Sqlite,
}

/// A world's storage that outlives the test worlds started on it.
struct Store {
    dir: TestDir,
    sqlite: Option<Arc<dyn WorldBackend>>,
}

impl Store {
    fn new(kind: Kind) -> Self {
        let dir = TestDir::new();
        let sqlite = match kind {
            Kind::Vanilla => None,
            Kind::Sqlite => Some(Arc::new(
                SqliteBackend::open(dir.path().join("world.mcserver")).unwrap(),
            ) as Arc<dyn WorldBackend>),
        };
        Self { dir, sqlite }
    }

    fn backend(&self) -> Arc<dyn WorldBackend> {
        match &self.sqlite {
            Some(backend) => backend.clone(),
            None => Storage::default().open(self.dir.path()).unwrap(),
        }
    }

    fn world(&self, with_map: bool) -> TestWorld {
        world_on(self.backend(), with_map)
    }
}

fn world_on(backend: Arc<dyn WorldBackend>, with_map: bool) -> TestWorld {
    let features: Vec<Box<dyn tuff_core::feature::Feature>> = if with_map {
        vec![Box::new(WorldMapFeature)]
    } else {
        Vec::new()
    };
    TestWorld::start_with_features_on(backend, features)
}

async fn blocking<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(job).await.unwrap()
}

fn map(world: &TestWorld) -> MapHandle {
    world
        .shared
        .services
        .get::<MapService>()
        .and_then(MapService::handle)
        .expect("the map service runs")
        .clone()
}

fn overworld(world: &TestWorld) -> u16 {
    world
        .shared
        .dimensions
        .iter()
        .position(|d| d.key == OVERWORLD)
        .unwrap() as u16
}

fn color(block: Block) -> u8 {
    block.default_state().map_color()
}

fn at(pixels: &[u8], x: usize, z: usize) -> u8 {
    pixels[z * SIDE + x]
}

async fn png_of(map: &MapHandle, key: TileKey, if_none_match: Option<String>) -> (String, Vec<u8>) {
    match map.tile(key, "test", if_none_match).await {
        TileReply::Png { etag, png: bytes } => {
            let pixels = png::decode(&bytes, SIDE).expect("a tile the map can read back");
            (etag, pixels)
        }
        other => panic!("expected a tile, got {other:?}"),
    }
}

/// Loads `chunk` (subscribing a player) and places `block` at the given
/// columns of it at y 200.
async fn build(world: &TestWorld, chunk: ChunkPos, block: Block, columns: &[(i32, i32)]) {
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let blocks: Vec<_> = columns
        .iter()
        .map(|(x, z)| (BlockPos::new(*x, 200, *z), block.default_state()))
        .collect();
    place_blocks(world, &region, blocks).await;
}

async fn persisted_index(backend: Arc<dyn WorldBackend>) -> disk::Loaded {
    blocking(move || disk::load(backend.as_ref())).await
}

/// Four blocks that make one pixel a level up.
const QUAD_4: [(i32, i32); 4] = [(4, 4), (5, 4), (4, 5), (5, 5)];
const QUAD_8: [(i32, i32); 4] = [(8, 8), (9, 8), (8, 9), (9, 9)];

fn overworld_path(level: u8, x: i32, z: i32) -> String {
    disk::tile_path(OVERWORLD, level, x, z)
}

async fn persisted_tiles_survive_a_restart_and_are_rechecked(kind: Kind) {
    let store = Store::new(kind);
    let a = |dim| TileKey::new(dim, 0, 0, 0);
    let b = |dim| TileKey::new(dim, 0, -1, 0);
    let zoomed = |dim| TileKey::new(dim, 1, 0, 0);
    {
        let world = store.world(true);
        let dim = overworld(&world);
        build(
            &world,
            ChunkPos::new(0, 0),
            Block::GoldBlock,
            &[(4, 4), (5, 5)],
        )
        .await;
        build(&world, ChunkPos::new(-1, 0), Block::GoldBlock, &[(-4, 4)]).await;
        let map = map(&world);
        for key in [a(dim), b(dim), zoomed(dim)] {
            png_of(&map, key, None).await;
        }
        map.flush().await;
        let stats = map.stats().await;
        assert_eq!(
            stats.disk_writes, 3,
            "{kind:?}: both tiles and the zoomed one"
        );
        assert_eq!(stats.disk_entries, 3);
        world.shutdown().await;
    }
    {
        let world = store.world(false);
        build(&world, ChunkPos::new(0, 0), Block::DiamondBlock, &QUAD_8).await;
        world.shutdown().await;
    }
    let world = store.world(true);
    let dim = overworld(&world);
    let map = map(&world);
    let (etag_a, served) = png_of(&map, a(dim), None).await;
    assert_eq!(at(&served, 4, 4) >> 2, color(Block::GoldBlock));
    assert_ne!(
        at(&served, 8, 8) >> 2,
        color(Block::DiamondBlock),
        "{kind:?}: served from the cache, before any check"
    );
    let (etag_b, served) = png_of(&map, b(dim), None).await;
    assert_eq!(at(&served, 512 - 4, 4) >> 2, color(Block::GoldBlock));
    let (etag_zoomed, served) = png_of(&map, zoomed(dim), None).await;
    assert_eq!(at(&served, 2, 2) >> 2, color(Block::GoldBlock));
    assert_ne!(at(&served, 4, 4) >> 2, color(Block::DiamondBlock));
    let stats = map.stats().await;
    assert_eq!((stats.disk_loads, stats.rendered), (3, 0), "{kind:?}");

    map.idle().await;
    let pixels = map.pixels(a(dim)).await.unwrap();
    assert_eq!(
        at(&pixels, 8, 8) >> 2,
        color(Block::DiamondBlock),
        "{kind:?}: the check finds the change"
    );
    assert_eq!(at(&pixels, 4, 4) >> 2, color(Block::GoldBlock));
    let pixels = map.pixels(zoomed(dim)).await.unwrap();
    assert_eq!(
        at(&pixels, 4, 4) >> 2,
        color(Block::DiamondBlock),
        "the zoomed tile is checked through its children"
    );
    let (fresh, _) = png_of(&map, a(dim), Some(etag_a.clone())).await;
    assert_ne!(fresh, etag_a, "a changed tile gets a new version");
    assert!(
        matches!(
            map.tile(b(dim), "test", Some(etag_b)).await,
            TileReply::NotModified { .. }
        ),
        "{kind:?}: an unchanged tile keeps its version"
    );
    let stats = map.stats().await;
    assert!(stats.rechecked >= 3, "{stats:?}");
    assert!((1..=2).contains(&stats.recheck_changed), "{stats:?}");
    let (fresh, _) = png_of(&map, zoomed(dim), Some(etag_zoomed.clone())).await;
    assert_ne!(
        fresh, etag_zoomed,
        "the zoomed tile is patched from its child"
    );

    map.flush().await;
    let backend = store.backend();
    let bytes = blocking(move || backend.read_extra_file(&overworld_path(0, 0, 0)).unwrap()).await;
    match disk::decode_tile(&bytes, OVERWORLD, TileKey::new(0, 0, 0, 0)) {
        Ok(Persisted::Base { tile, .. }) => {
            assert_eq!(at(&tile.pixels, 8, 8) >> 2, color(Block::DiamondBlock))
        }
        _ => panic!("the checked tile is persisted again"),
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vanilla_tiles_survive_a_restart_and_are_rechecked() {
    persisted_tiles_survive_a_restart_and_are_rechecked(Kind::Vanilla).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_file_tiles_survive_a_restart_and_are_rechecked() {
    persisted_tiles_survive_a_restart_and_are_rechecked(Kind::Sqlite).await;
}

async fn a_corrupt_entry_is_ignored(kind: Kind) {
    let store = Store::new(kind);
    {
        let world = store.world(true);
        build(&world, ChunkPos::new(0, 0), Block::GoldBlock, &[(4, 4)]).await;
        let map = map(&world);
        png_of(&map, TileKey::new(overworld(&world), 0, 0, 0), None).await;
        map.flush().await;
        assert_eq!(map.stats().await.disk_writes, 1);
        world.shutdown().await;
    }
    let backend = store.backend();
    blocking(move || {
        let path = overworld_path(0, 0, 0);
        let mut bytes = backend.read_extra_file(&path).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0x55;
        backend.write_extra_file(&path, &bytes).unwrap();
    })
    .await;
    let world = store.world(true);
    let map = map(&world);
    let key = TileKey::new(overworld(&world), 0, 0, 0);
    let (_, pixels) = png_of(&map, key, None).await;
    assert_eq!(
        at(&pixels, 4, 4) >> 2,
        color(Block::GoldBlock),
        "{kind:?}: drawn from the world instead"
    );
    let stats = map.stats().await;
    assert_eq!(
        (stats.disk_errors, stats.disk_loads, stats.rendered),
        (1, 0, 1),
        "{kind:?}"
    );
    map.flush().await;
    let backend = store.backend();
    let bytes = blocking(move || backend.read_extra_file(&overworld_path(0, 0, 0)).unwrap()).await;
    assert!(
        disk::decode_tile(&bytes, OVERWORLD, TileKey::new(0, 0, 0, 0)).is_ok(),
        "the entry is written again"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_corrupt_vanilla_entry_is_ignored() {
    a_corrupt_entry_is_ignored(Kind::Vanilla).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_corrupt_server_file_entry_is_ignored() {
    a_corrupt_entry_is_ignored(Kind::Sqlite).await;
}

async fn a_held_save_gate_defers_writes(kind: Kind) {
    let store = Store::new(kind);
    let world = store.world(true);
    let dim = overworld(&world);
    build(&world, ChunkPos::new(0, 0), Block::GoldBlock, &QUAD_4).await;
    let map = map(&world);
    png_of(&map, TileKey::new(dim, 0, 0, 0), None).await;
    let hold = hold_saves(&world.shared, "test", Duration::from_secs(60))
        .await
        .expect("saving is held");
    map.flush().await;
    assert_eq!(
        map.stats().await.disk_writes,
        0,
        "{kind:?}: nothing written while held"
    );
    let (_, pixels) = png_of(&map, TileKey::new(dim, 1, 0, 0), None).await;
    assert_eq!(
        at(&pixels, 2, 2) >> 2,
        color(Block::GoldBlock),
        "tiles are still served"
    );
    hold.release().await;
    map.flush().await;
    let stats = map.stats().await;
    assert_eq!(stats.disk_writes, 2, "{kind:?}: written once released");
    let loaded = persisted_index(store.backend()).await;
    assert_eq!(loaded.index.entries.len(), 2);
    assert!(loaded.orphans.is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_save_gate_defers_vanilla_writes() {
    a_held_save_gate_defers_writes(Kind::Vanilla).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_save_gate_defers_server_file_writes() {
    a_held_save_gate_defers_writes(Kind::Sqlite).await;
}

/// A backend whose extra-file writes are refused as the save gate refuses
/// them once `allowed` runs out.
struct Refusing {
    inner: Arc<dyn WorldBackend>,
    allowed: Arc<AtomicUsize>,
}

impl Refusing {
    fn admit(&self) -> io::Result<()> {
        self.allowed
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .map(drop)
            .map_err(|_| io::Error::new(io::ErrorKind::ResourceBusy, "saving is held"))
    }
}

impl WorldBackend for Refusing {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn lock(&self) -> io::Result<Box<dyn WorldLock>> {
        self.inner.lock()
    }
    fn documents(&self) -> &dyn Documents {
        self.inner.documents()
    }
    fn open_region(
        &self,
        dimension: &str,
        region: FileRegionPos,
    ) -> io::Result<Box<dyn RegionStore>> {
        self.inner.open_region(dimension, region)
    }
    fn blob_codec(&self) -> Arc<dyn tuff_storage::backend::BlobCodec> {
        self.inner.blob_codec()
    }
    fn list_dimensions(&self) -> io::Result<Vec<String>> {
        self.inner.list_dimensions()
    }
    fn list_regions(&self, dimension: &str) -> io::Result<Vec<FileRegionPos>> {
        self.inner.list_regions(dimension)
    }
    fn list_extra_files(&self) -> io::Result<Vec<String>> {
        self.inner.list_extra_files()
    }
    fn read_extra_file(&self, path: &str) -> io::Result<Vec<u8>> {
        self.inner.read_extra_file(path)
    }
    fn list_extra_files_under(&self, prefix: &str) -> io::Result<Vec<String>> {
        self.inner.list_extra_files_under(prefix)
    }
    fn read_extra_file_capped(&self, path: &str, max: usize) -> io::Result<Vec<u8>> {
        self.inner.read_extra_file_capped(path, max)
    }
    fn write_extra_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
        self.admit()?;
        self.inner.write_extra_file(path, data)
    }
    fn remove_extra_file(&self, path: &str) -> io::Result<()> {
        self.admit()?;
        self.inner.remove_extra_file(path)
    }
}

async fn refused_writes_break_nothing(kind: Kind) {
    let store = Store::new(kind);
    let allowed = Arc::new(AtomicUsize::new(usize::MAX));
    let refusing = |store: &Store| -> Arc<dyn WorldBackend> {
        Arc::new(Refusing {
            inner: store.backend(),
            allowed: allowed.clone(),
        })
    };
    let a = |dim| TileKey::new(dim, 0, 0, 0);
    let b = |dim| TileKey::new(dim, 0, -1, 0);
    {
        let world = world_on(refusing(&store), true);
        let dim = overworld(&world);
        build(&world, ChunkPos::new(0, 0), Block::GoldBlock, &[(4, 4)]).await;
        build(&world, ChunkPos::new(-1, 0), Block::GoldBlock, &[(-4, 4)]).await;
        let map = map(&world);
        png_of(&map, a(dim), None).await;
        png_of(&map, b(dim), None).await;
        allowed.store(1, Ordering::SeqCst);
        map.flush().await;
        assert_eq!(
            map.stats().await.disk_writes,
            1,
            "{kind:?}: stopped at the refusal"
        );
        let loaded = persisted_index(store.backend()).await;
        assert!(loaded.index.entries.is_empty(), "no index was written");
        assert_eq!(loaded.orphans.len(), 1);
        let (_, pixels) = png_of(&map, a(dim), None).await;
        assert_eq!(at(&pixels, 4, 4) >> 2, color(Block::GoldBlock));

        allowed.store(usize::MAX, Ordering::SeqCst);
        map.flush().await;
        assert_eq!(
            map.stats().await.disk_writes,
            2,
            "{kind:?}: the rest follows"
        );
        let loaded = persisted_index(store.backend()).await;
        assert_eq!(loaded.index.entries.len(), 2);
        assert!(loaded.orphans.is_empty());
        world.shutdown().await;
    }
    let world = world_on(refusing(&store), true);
    let dim = overworld(&world);
    let map = map(&world);
    let (_, pixels) = png_of(&map, a(dim), None).await;
    assert_eq!(at(&pixels, 4, 4) >> 2, color(Block::GoldBlock));
    png_of(&map, b(dim), None).await;
    let stats = map.stats().await;
    assert_eq!((stats.disk_loads, stats.disk_errors), (2, 0), "{kind:?}");
    map.idle().await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_vanilla_writes_break_nothing() {
    refused_writes_break_nothing(Kind::Vanilla).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_server_file_writes_break_nothing() {
    refused_writes_break_nothing(Kind::Sqlite).await;
}

async fn the_budget_keeps_the_most_recently_used(kind: Kind) {
    let store = Store::new(kind);
    let world = store.world(true);
    let dim = overworld(&world);
    build(&world, ChunkPos::new(0, 0), Block::GoldBlock, &[(4, 4)]).await;
    build(&world, ChunkPos::new(-1, 0), Block::GoldBlock, &[(-4, 4)]).await;
    let map = map(&world);
    png_of(&map, TileKey::new(dim, 0, -1, 0), None).await;
    png_of(&map, TileKey::new(dim, 0, 0, 0), None).await;
    map.flush().await;
    png_of(&map, TileKey::new(dim, 0, -1, 0), None).await;
    map.flush().await;
    let loaded = persisted_index(store.backend()).await;
    let index = &loaded.index;
    let overworld = index.dims.iter().position(|d| d == OVERWORLD).unwrap() as u16;
    let used = |x| {
        index.entries[&DiskKey {
            dim: overworld,
            level: 0,
            x,
            z: 0,
        }]
            .used
    };
    assert!(
        used(-1) > used(0),
        "{kind:?}: the tile served last is evicted last"
    );

    world.shared.config.set(DISK_KEY, Value::Int(0)).unwrap();
    map.flush().await;
    assert_eq!(map.stats().await.disk_entries, 0);
    let backend = store.backend();
    let left = blocking(move || backend.list_extra_files_under(disk::ROOT).unwrap()).await;
    assert!(
        left.iter().all(|p| disk::SLOTS.contains(&p.as_str())),
        "{kind:?}: a zero budget deletes every tile: {left:?}"
    );
    assert!(
        persisted_index(store.backend())
            .await
            .index
            .entries
            .is_empty()
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_vanilla_budget_keeps_the_most_recently_used() {
    the_budget_keeps_the_most_recently_used(Kind::Vanilla).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_file_budget_keeps_the_most_recently_used() {
    the_budget_keeps_the_most_recently_used(Kind::Sqlite).await;
}

async fn a_restored_backup_without_the_cache_serves_the_map(kind: Kind) {
    use tuff_storage::backup::{Plan, Tracker, run_offline};
    let store = Store::new(kind);
    let key = |dim| TileKey::new(dim, 0, 0, 0);
    {
        let world = store.world(true);
        let dim = overworld(&world);
        build(&world, ChunkPos::new(0, 0), Block::GoldBlock, &[(4, 4)]).await;
        let map = map(&world);
        png_of(&map, key(dim), None).await;
        map.flush().await;
        assert_eq!(map.stats().await.disk_writes, 1);
        world.shutdown().await;
    }
    let out = TestDir::new();
    let plan = Plan {
        source: store.backend().backup_source(),
        dir: out.path().join("backups"),
        level: "world".to_owned(),
        compression: 1,
        staged: true,
        include_caches: false,
    };
    let (tracker, _) = Tracker::new();
    let (name, _) = blocking(move || run_offline(&plan, &tracker))
        .await
        .unwrap();
    let path = out.path().join("backups").join(name);
    let restored: Arc<dyn WorldBackend> = match kind {
        Kind::Sqlite => Arc::new(SqliteBackend::open(&path).unwrap()),
        Kind::Vanilla => {
            let into = out.path().join("restored");
            let archive = tuff_zip::Archive::open(
                std::fs::File::open(&path).unwrap(),
                tuff_zip::Limits::untrusted(1 << 30),
            )
            .unwrap();
            std::fs::create_dir_all(&into).unwrap();
            archive.extract_to(&into, |_| Ok(())).unwrap();
            Storage::default().open(&into.join("world")).unwrap()
        }
    };
    assert!(
        restored
            .list_extra_files_under("tuff/world-map/")
            .unwrap()
            .is_empty(),
        "{kind:?}: the backup carries no map cache"
    );
    let world = world_on(restored, true);
    let dim = overworld(&world);
    let map = map(&world);
    let (_, served) = png_of(&map, key(dim), None).await;
    assert_eq!(at(&served, 4, 4) >> 2, color(Block::GoldBlock), "{kind:?}");
    let stats = map.stats().await;
    assert_eq!((stats.disk_loads, stats.rendered), (0, 1), "{kind:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restored_vanilla_backup_without_the_cache_serves_the_map() {
    a_restored_backup_without_the_cache_serves_the_map(Kind::Vanilla).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restored_server_file_backup_without_the_cache_serves_the_map() {
    a_restored_backup_without_the_cache_serves_the_map(Kind::Sqlite).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rechecking_a_stale_high_level_tile_is_bounded_per_view() {
    use tuff_feature_builtin_world_map::service::RECHECK_BASE_BUDGET;
    const TILES: usize = RECHECK_BASE_BUDGET + 4;
    let store = Store::new(Kind::Vanilla);
    let top = |dim| TileKey::new(dim, 3, 0, 0);
    let chunk_of = |i: usize| ChunkPos::new((i % 8) as i32 * 32, (i / 8) as i32 * 32);
    let shifted = |i: usize, columns: &[(i32, i32)]| -> Vec<(i32, i32)> {
        let c = chunk_of(i);
        columns
            .iter()
            .map(|(x, z)| (x + c.x * 16, z + c.z * 16))
            .collect()
    };
    {
        let world = store.world(true);
        let dim = overworld(&world);
        for i in 0..TILES {
            build(
                &world,
                chunk_of(i),
                Block::GoldBlock,
                &shifted(i, &[(4, 4)]),
            )
            .await;
        }
        let map = map(&world);
        png_of(&map, top(dim), None).await;
        map.flush().await;
        world.shutdown().await;
    }
    {
        let world = store.world(false);
        for i in 0..TILES {
            build(
                &world,
                chunk_of(i),
                Block::DiamondBlock,
                &shifted(i, &QUAD_8),
            )
            .await;
        }
        world.shutdown().await;
    }
    let world = store.world(true);
    let dim = overworld(&world);
    let map = map(&world);
    png_of(&map, top(dim), None).await;
    map.idle().await;
    let stats = map.stats().await;
    assert!(stats.recheck_deferred > 0, "{stats:?}");
    let mut changed = 0;
    for i in 0..TILES {
        let key = TileKey::new(dim, 0, (i % 8) as i32, (i / 8) as i32);
        if let Some(pixels) = map.pixels(key).await
            && at(&pixels, 8, 8) >> 2 == color(Block::DiamondBlock)
        {
            changed += 1;
        }
    }
    assert_eq!(
        changed, RECHECK_BASE_BUDGET,
        "only the budget was rechecked"
    );

    png_of(&map, top(dim), None).await;
    map.idle().await;
    for i in 0..TILES {
        let key = TileKey::new(dim, 0, (i % 8) as i32, (i / 8) as i32);
        let pixels = map.pixels(key).await.unwrap();
        assert_eq!(
            at(&pixels, 8, 8) >> 2,
            color(Block::DiamondBlock),
            "tile {i} is reached by the second view"
        );
    }
    world.shutdown().await;
}
