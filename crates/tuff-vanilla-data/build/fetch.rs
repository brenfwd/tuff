//! Downloads the vanilla server jar and a matching Mojang Java runtime, then
//! runs the vanilla data generator in reports-only mode (no server is started).
//! Everything is cached, so once a version has been prepared no network access
//! is needed.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use sha1::{Digest, Sha1};

use crate::cache;

pub const CACHE_DIR_ENV: &str = "VANILLA_CACHE_DIR";
/// Overrides the downloaded Mojang runtime, e.g. on hosts Mojang does not ship
/// one for. It must be new enough for the requested game version.
pub const JAVA_ENV: &str = "VANILLA_JAVA";

const VERSION_MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const JAVA_RUNTIMES: &str = "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";
const DOWNLOAD_WORKERS: usize = 16;

pub struct Vanilla {
    pub server_jar: PathBuf,
    pub reports: PathBuf,
    /// Output of `LightProperties.java`: the block state properties the light
    /// engine reads, which no data generator report contains.
    pub light: PathBuf,
    /// Output of `BlockProperties.java`: other block state properties no
    /// report contains.
    pub block_properties: PathBuf,
    /// Output of `ItemProperties.java`: item data that lives in game code.
    pub item_properties: PathBuf,
    /// Output of `GameRuleProperties.java`: game rule types and defaults.
    pub game_rules: PathBuf,
    /// Output of `WorldProperties.java`: world rules that live in code.
    pub world_properties: PathBuf,
    /// Output of `EntityProperties.java`: entity type data that lives in code.
    pub entity_properties: PathBuf,
    /// Output of `AttributeProperties.java`: attribute ranges, entity base
    /// values and mob effect modifiers.
    pub attribute_properties: PathBuf,
    /// Output of `BlockEntityProperties.java`: which blocks have block
    /// entities and what new ones hold.
    pub block_entity_properties: PathBuf,
    /// Output of `FluidProperties.java`: fluid rules and the block state
    /// properties the fluid simulation reads.
    pub fluid_properties: PathBuf,
    /// Output of `DragonProperties.java`: the ender dragon's phase ids and
    /// flight graph.
    pub dragon_properties: PathBuf,
    /// Output of `GrowthProperties.java`: random ticks, growth, bone meal
    /// and fire data.
    pub growth_properties: PathBuf,
    /// Output of `PotionProperties.java`: potion effects and mob effect
    /// categories, colors and particles.
    pub potion_properties: PathBuf,
    /// Output of `VillagerProperties.java`: professions, points of
    /// interest, villager types by biome, levels and gossip.
    pub villager_properties: PathBuf,
    /// Output of `RedstoneProperties.java`: signals, conductors, comparator
    /// inputs and redstone components.
    pub redstone_properties: PathBuf,
    /// Output of `MapProperties.java`: map colours and decoration types.
    pub map_properties: PathBuf,
    /// Output of `NetworkRegistries.java`: the data registries synchronized
    /// to clients, in send order.
    pub network_registries: PathBuf,
}

/// A Java source file compiled and run against the extracted server jar. Its
/// output file is named by the source hash, so editing a dumper reruns it
/// without rerunning the data generator.
struct Dumper {
    class: &'static str,
    prefix: &'static str,
    source: &'static str,
}

impl Dumper {
    fn output(&self, dir: &Path) -> PathBuf {
        dir.join(format!(
            "{}-{}.txt",
            self.prefix,
            sha1_hex(self.source.as_bytes())
        ))
    }
}

const LIGHT_DUMPER: Dumper = Dumper {
    class: "LightProperties",
    prefix: "light",
    source: include_str!("LightProperties.java"),
};
const BLOCK_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "BlockProperties",
    prefix: "block-properties",
    source: include_str!("BlockProperties.java"),
};
const ITEM_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "ItemProperties",
    prefix: "item-properties",
    source: include_str!("ItemProperties.java"),
};
const GAME_RULES_DUMPER: Dumper = Dumper {
    class: "GameRuleProperties",
    prefix: "game-rules",
    source: include_str!("GameRuleProperties.java"),
};
const WORLD_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "WorldProperties",
    prefix: "world-properties",
    source: include_str!("WorldProperties.java"),
};
const ENTITY_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "EntityProperties",
    prefix: "entity-properties",
    source: include_str!("EntityProperties.java"),
};
const ATTRIBUTE_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "AttributeProperties",
    prefix: "attribute-properties",
    source: include_str!("AttributeProperties.java"),
};
const BLOCK_ENTITY_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "BlockEntityProperties",
    prefix: "block-entity-properties",
    source: include_str!("BlockEntityProperties.java"),
};
const FLUID_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "FluidProperties",
    prefix: "fluid-properties",
    source: include_str!("FluidProperties.java"),
};
const POTION_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "PotionProperties",
    prefix: "potion-properties",
    source: include_str!("PotionProperties.java"),
};
const VILLAGER_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "VillagerProperties",
    prefix: "villager-properties",
    source: include_str!("VillagerProperties.java"),
};
const MAP_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "MapProperties",
    prefix: "map-properties",
    source: include_str!("MapProperties.java"),
};
const REDSTONE_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "RedstoneProperties",
    prefix: "redstone-properties",
    source: include_str!("RedstoneProperties.java"),
};
const GROWTH_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "GrowthProperties",
    prefix: "growth-properties",
    source: include_str!("GrowthProperties.java"),
};
const NETWORK_REGISTRIES_DUMPER: Dumper = Dumper {
    class: "NetworkRegistries",
    prefix: "network-registries",
    source: include_str!("NetworkRegistries.java"),
};
const DRAGON_PROPERTIES_DUMPER: Dumper = Dumper {
    class: "DragonProperties",
    prefix: "dragon-properties",
    source: include_str!("DragonProperties.java"),
};

pub fn prepare(version: &str, cache_dir: &Path) -> Result<Vanilla> {
    let dir = cache_dir.join(version);
    let vanilla = Vanilla {
        server_jar: dir.join("server.jar"),
        reports: dir.join("generated").join("reports"),
        light: LIGHT_DUMPER.output(&dir),
        block_properties: BLOCK_PROPERTIES_DUMPER.output(&dir),
        item_properties: ITEM_PROPERTIES_DUMPER.output(&dir),
        game_rules: GAME_RULES_DUMPER.output(&dir),
        world_properties: WORLD_PROPERTIES_DUMPER.output(&dir),
        entity_properties: ENTITY_PROPERTIES_DUMPER.output(&dir),
        attribute_properties: ATTRIBUTE_PROPERTIES_DUMPER.output(&dir),
        block_entity_properties: BLOCK_ENTITY_PROPERTIES_DUMPER.output(&dir),
        fluid_properties: FLUID_PROPERTIES_DUMPER.output(&dir),
        dragon_properties: DRAGON_PROPERTIES_DUMPER.output(&dir),
        growth_properties: GROWTH_PROPERTIES_DUMPER.output(&dir),
        potion_properties: POTION_PROPERTIES_DUMPER.output(&dir),
        villager_properties: VILLAGER_PROPERTIES_DUMPER.output(&dir),
        redstone_properties: REDSTONE_PROPERTIES_DUMPER.output(&dir),
        map_properties: MAP_PROPERTIES_DUMPER.output(&dir),
        network_registries: NETWORK_REGISTRIES_DUMPER.output(&dir),
    };
    let complete = dir.join("generated.complete");
    let dumpers = [
        (&LIGHT_DUMPER, &vanilla.light),
        (&BLOCK_PROPERTIES_DUMPER, &vanilla.block_properties),
        (&ITEM_PROPERTIES_DUMPER, &vanilla.item_properties),
        (&GAME_RULES_DUMPER, &vanilla.game_rules),
        (&WORLD_PROPERTIES_DUMPER, &vanilla.world_properties),
        (&ENTITY_PROPERTIES_DUMPER, &vanilla.entity_properties),
        (&ATTRIBUTE_PROPERTIES_DUMPER, &vanilla.attribute_properties),
        (
            &BLOCK_ENTITY_PROPERTIES_DUMPER,
            &vanilla.block_entity_properties,
        ),
        (&FLUID_PROPERTIES_DUMPER, &vanilla.fluid_properties),
        (&DRAGON_PROPERTIES_DUMPER, &vanilla.dragon_properties),
        (&GROWTH_PROPERTIES_DUMPER, &vanilla.growth_properties),
        (&POTION_PROPERTIES_DUMPER, &vanilla.potion_properties),
        (&VILLAGER_PROPERTIES_DUMPER, &vanilla.villager_properties),
        (&REDSTONE_PROPERTIES_DUMPER, &vanilla.redstone_properties),
        (&MAP_PROPERTIES_DUMPER, &vanilla.map_properties),
        (&NETWORK_REGISTRIES_DUMPER, &vanilla.network_registries),
    ];
    let work = dir.join("work");
    if is_ready(&vanilla, &complete, &dumpers) {
        return Ok(vanilla);
    }

    // Everything below touches the cache in ways concurrent builds (several
    // git worktrees can share one VANILLA_CACHE_DIR) must not race: hold the
    // lock across the whole regeneration, including the dumper loop, and
    // recheck readiness under it in case another process just finished.
    let _lock = cache::Lock::acquire(cache_dir)
        .with_context(|| format!("locking the vanilla cache {}", cache_dir.display()))?;
    if is_ready(&vanilla, &complete, &dumpers) {
        return Ok(vanilla);
    }

    let http = Http::new()?;
    let manifest: VersionManifest = http.json(VERSION_MANIFEST)?;
    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == version)
        .ok_or_else(|| anyhow!("version {version} is not in the Mojang version manifest"))?;
    let meta: VersionMeta = http.json(&entry.url)?;
    let server = &meta.downloads.server;
    http.download(&server.url, &server.sha1, &vanilla.server_jar)?;

    let java = match std::env::var_os(JAVA_ENV) {
        Some(java) => PathBuf::from(java),
        None => install_java(&http, &meta.java_version.component, &cache_dir.join("java"))?,
    };
    let dumpers_ready = dumpers.iter().all(|(_, output)| output.is_file());
    // Rerun the data generator, which extracts the server jar's libraries
    // into `work/` as a side effect, whenever `generated.complete` is unset
    // or some dumper still needs `work/` and it is gone: the reports under
    // `generated/` never depend on `work/` surviving, but the dumpers below
    // do, and something outside this build (another build, a disk cleanup)
    // may have removed it after the reports were cached.
    if !complete.is_file() || (!dumpers_ready && !cache::work_has_jars(&work)) {
        run_data_generator(&java, &vanilla.server_jar, &dir)?;
        cache::write_atomic(&complete, b"")?;
    }
    if !dumpers_ready {
        ensure!(
            cache::work_has_jars(&work),
            "the data generator ran but {} still has no extracted server jar; \
             something outside this build removed it mid-run",
            work.display(),
        );
    }
    for (dumper, output) in dumpers {
        if !output.is_file() {
            run_dumper(&java, &dir, dumper, output)?;
        }
    }
    Ok(vanilla)
}

fn is_ready(vanilla: &Vanilla, complete: &Path, dumpers: &[(&Dumper, &PathBuf)]) -> bool {
    vanilla.server_jar.is_file()
        && complete.is_file()
        && dumpers.iter().all(|(_, output)| output.is_file())
}

/// Runs a dumper in single-file source mode with the jars the server bundler
/// extracted into `work/` (while running the data generator) as the
/// classpath, so no version or library path is hardcoded.
fn run_dumper(java: &Path, dir: &Path, dumper: &Dumper, output: &Path) -> Result<()> {
    let work = dir.join("work");
    let mut jars = Vec::new();
    for sub in ["versions", "libraries"] {
        cache::collect_jars(&work.join(sub), &mut jars)?;
    }
    ensure!(
        jars.iter()
            .any(|jar| jar.starts_with(work.join("versions"))),
        "no extracted server jar under {}; delete {} to rerun the data generator",
        work.display(),
        dir.join("generated.complete").display(),
    );
    jars.sort();
    let classpath = std::env::join_paths(&jars)?;

    let temp = cache::TempDir::new_in(dir)?;
    let source = temp.path().join(format!("{}.java", dumper.class));
    fs::write(&source, dumper.source)?;
    let partial = temp.path().join("output.txt");
    let result = Command::new(java)
        .arg("-cp")
        .arg(&classpath)
        .arg(&source)
        .arg(&partial)
        .current_dir(temp.path())
        .output()
        .with_context(|| format!("failed to run {}", java.display()))?;
    ensure!(
        result.status.success(),
        "build/{}.java failed ({}). If this is a new game version, \
         a method it calls was probably renamed or moved.\n{}\n{}",
        dumper.class,
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
    cache::publish(&partial, output)?;
    Ok(())
}

fn sha1_hex(data: &[u8]) -> String {
    Sha1::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn run_data_generator(java: &Path, jar: &Path, dir: &Path) -> Result<()> {
    let generated = dir.join("generated");
    if generated.exists() {
        fs::remove_dir_all(&generated)?;
    }
    let work = dir.join("work");
    fs::create_dir_all(&work)?;
    let output = Command::new(java)
        .arg("-DbundlerMainClass=net.minecraft.data.Main")
        .arg("-jar")
        .arg(jar)
        .arg("--reports")
        .arg("--output")
        .arg(&generated)
        .current_dir(&work)
        .output()
        .with_context(|| format!("failed to run {}", java.display()))?;
    ensure!(
        output.status.success(),
        "data generator failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(())
}

fn install_java(http: &Http, component: &str, root: &Path) -> Result<PathBuf> {
    let dir = root.join(component);
    let marker = dir.join(".java-path");
    if let Ok(relative) = fs::read_to_string(&marker) {
        return Ok(dir.join(relative));
    }

    let platform = runtime_platform()?;
    let runtimes: HashMap<String, HashMap<String, Vec<Runtime>>> = http.json(JAVA_RUNTIMES)?;
    let runtime = runtimes
        .get(platform)
        .and_then(|components| components.get(component))
        .and_then(|builds| builds.first())
        .ok_or_else(|| {
            anyhow!("Mojang ships no {component} runtime for {platform}; set {JAVA_ENV}")
        })?;
    let manifest: RuntimeManifest = http.json(&runtime.manifest.url)?;

    let mut files = Vec::new();
    for (name, file) in &manifest.files {
        let path = dir.join(name);
        match file {
            RuntimeFile::Directory => fs::create_dir_all(&path)?,
            RuntimeFile::File {
                downloads,
                executable,
            } => files.push((path, &downloads.raw, *executable)),
            RuntimeFile::Link { target } => link(target, &path)?,
        }
    }
    parallel(&files, |(path, raw, executable)| {
        http.download(&raw.url, &raw.sha1, path)?;
        if *executable {
            make_executable(path)?;
        }
        Ok(())
    })?;

    let relative = manifest
        .files
        .iter()
        .find(|(name, file)| {
            matches!(file, RuntimeFile::File { .. })
                && (name.ends_with("bin/java") || name.ends_with("bin/java.exe"))
        })
        .map(|(name, _)| name)
        .ok_or_else(|| anyhow!("no java executable in the {component} runtime"))?;
    cache::write_atomic(&marker, relative.as_bytes())?;
    Ok(dir.join(relative))
}

fn runtime_platform() -> Result<&'static str> {
    use std::env::consts::{ARCH, OS};
    Ok(match (OS, ARCH) {
        ("macos", "aarch64") => "mac-os-arm64",
        ("macos", _) => "mac-os",
        ("linux", "x86") => "linux-i386",
        ("linux", "x86_64") => "linux",
        ("windows", "aarch64") => "windows-arm64",
        ("windows", "x86") => "windows-x86",
        ("windows", "x86_64") => "windows-x64",
        _ => bail!("no Mojang Java runtime for {OS}/{ARCH}; set {JAVA_ENV}"),
    })
}

#[cfg(unix)]
fn link(target: &str, path: &Path) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(target, path)?;
    Ok(())
}

#[cfg(not(unix))]
fn link(_target: &str, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Runs `f` over `items` on a fixed pool of threads. Downloads are
/// latency-bound, so the pool size does not depend on the CPU count.
fn parallel<T: Sync>(items: &[T], f: impl Fn(&T) -> Result<()> + Sync) -> Result<()> {
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        let workers: Vec<_> = (0..DOWNLOAD_WORKERS)
            .map(|_| {
                scope.spawn(|| {
                    while let Some(item) = items.get(next.fetch_add(1, Ordering::Relaxed)) {
                        f(item)?;
                    }
                    Ok(())
                })
            })
            .collect();
        workers
            .into_iter()
            .try_for_each(|worker| worker.join().expect("download worker panicked"))
    })
}

struct Http(reqwest::blocking::Client);

impl Http {
    fn new() -> Result<Self> {
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls =
            rustls::ClientConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
                .with_safe_default_protocol_versions()?
                .with_root_certificates(roots)
                .with_no_client_auth();
        let client = reqwest::blocking::Client::builder()
            .tls_backend_preconfigured(tls)
            .user_agent("vanilla-data-build")
            .timeout(Duration::from_secs(600))
            .build()?;
        Ok(Self(client))
    }

    fn bytes(&self, url: &str) -> Result<Vec<u8>> {
        let response = self
            .0
            .get(url)
            .send()
            .and_then(|response| response.error_for_status())
            .with_context(|| format!("GET {url}"))?;
        Ok(response.bytes()?.to_vec())
    }

    fn json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        serde_json::from_slice(&self.bytes(url)?).with_context(|| format!("parsing {url}"))
    }

    /// Files only appear at `dest` after their hash has been verified, so an
    /// existing file is trusted without re-hashing.
    fn download(&self, url: &str, sha1: &str, dest: &Path) -> Result<()> {
        if dest.is_file() {
            return Ok(());
        }
        let data = self.bytes(url)?;
        let actual = sha1_hex(&data);
        ensure!(
            actual == sha1,
            "sha1 mismatch for {url}: expected {sha1}, got {actual}"
        );
        cache::write_atomic(dest, &data)?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct VersionManifest {
    versions: Vec<VersionEntry>,
}

#[derive(Deserialize)]
struct VersionEntry {
    id: String,
    url: String,
}

#[derive(Deserialize)]
struct VersionMeta {
    downloads: VersionDownloads,
    #[serde(rename = "javaVersion")]
    java_version: JavaVersion,
}

#[derive(Deserialize)]
struct VersionDownloads {
    server: Download,
}

#[derive(Deserialize)]
struct JavaVersion {
    component: String,
}

#[derive(Deserialize)]
struct Download {
    url: String,
    sha1: String,
}

#[derive(Deserialize)]
struct Runtime {
    manifest: Download,
}

#[derive(Deserialize)]
struct RuntimeManifest {
    files: BTreeMap<String, RuntimeFile>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum RuntimeFile {
    Directory,
    File {
        downloads: RuntimeDownloads,
        #[serde(default)]
        executable: bool,
    },
    Link {
        target: String,
    },
}

#[derive(Deserialize)]
struct RuntimeDownloads {
    raw: Download,
}
