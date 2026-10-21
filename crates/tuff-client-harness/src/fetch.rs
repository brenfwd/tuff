use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use sha1::{Digest, Sha1};

use crate::cache;
use crate::meta::{AssetIndex, Platform, VersionMeta};

pub const CACHE_DIR_ENV: &str = "VANILLA_CACHE_DIR";
pub const JAVA_ENV: &str = "VANILLA_JAVA";
pub const ASSETS_ENV: &str = "TUFF_CLIENT_ASSETS";

const VERSION_MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const ASSET_HOST: &str = "https://resources.download.minecraft.net";
const WORKERS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetProfile {
    /// Every asset object (the default).
    Full,
    /// Skips `minecraft/sounds/**` except `sounds.json`, and `minecraft/lang/**`.
    Minimal,
}

impl AssetProfile {
    pub fn from_env() -> Result<Self> {
        match std::env::var(ASSETS_ENV).as_deref() {
            Err(_) | Ok("") | Ok("full") => Ok(Self::Full),
            Ok("minimal") => Ok(Self::Minimal),
            Ok(other) => bail!("{ASSETS_ENV} must be `full` or `minimal`, got `{other}`"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Minimal => "minimal",
        }
    }

    pub fn includes(self, name: &str) -> bool {
        match self {
            Self::Full => true,
            Self::Minimal => {
                !name.starts_with("minecraft/lang/")
                    && (!name.starts_with("minecraft/sounds/") || name == "minecraft/sounds.json")
            }
        }
    }
}

pub trait Source: Sync {
    fn get(&self, url: &str) -> Result<Vec<u8>>;
}

pub struct HttpSource(reqwest::blocking::Client);

impl HttpSource {
    pub fn new() -> Result<Self> {
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls =
            rustls::ClientConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
                .with_safe_default_protocol_versions()?
                .with_root_certificates(roots)
                .with_no_client_auth();
        Ok(Self(
            reqwest::blocking::Client::builder()
                .tls_backend_preconfigured(tls)
                .user_agent("tuff-client-harness")
                .timeout(Duration::from_secs(600))
                .build()?,
        ))
    }
}

impl Source for HttpSource {
    fn get(&self, url: &str) -> Result<Vec<u8>> {
        let response = self
            .0
            .get(url)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("GET {url}"))?;
        Ok(response.bytes()?.to_vec())
    }
}

#[derive(Debug)]
pub struct ClientFiles {
    pub version: String,
    pub meta: VersionMeta,
    pub client_jar: PathBuf,
    /// Libraries in classpath order, followed by the client jar.
    pub classpath: Vec<PathBuf>,
    pub natives_dir: PathBuf,
    pub assets_dir: PathBuf,
    pub asset_index_id: String,
}

pub fn cache_dir() -> PathBuf {
    match std::env::var_os(CACHE_DIR_ENV) {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/vanilla"),
    }
}

/// `VANILLA_JAVA`, else the runtime vanilla-data installed during any build.
pub fn resolve_java(cache: &Path, component: &str) -> Result<PathBuf> {
    if let Some(java) = std::env::var_os(JAVA_ENV) {
        return Ok(PathBuf::from(java));
    }
    let dir = cache.join("java").join(component);
    let relative = fs::read_to_string(dir.join(".java-path")).map_err(|_| {
        anyhow!(
            "no Java runtime {component} under {}: build the workspace once or set {JAVA_ENV}",
            cache.display()
        )
    })?;
    Ok(dir.join(relative))
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

fn sha1_hex(data: &[u8]) -> String {
    Sha1::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn download(source: &dyn Source, url: &str, sha1: &str, dest: &Path) -> Result<()> {
    if dest.is_file() {
        return Ok(());
    }
    let data = source.get(url)?;
    let actual = sha1_hex(&data);
    ensure!(
        actual == sha1,
        "sha1 mismatch for {url}: expected {sha1}, got {actual}"
    );
    cache::write_atomic(dest, &data)?;
    Ok(())
}

fn parallel<T: Sync>(items: &[T], work: impl Fn(&T) -> Result<()> + Sync) -> Result<()> {
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    thread::scope(|scope| {
        let workers: Vec<_> = (0..WORKERS.min(items.len()))
            .map(|n| {
                let worker = thread::Builder::new().name(format!("client-fetch-{n}"));
                worker
                    .spawn_scoped(scope, || {
                        while !failed.load(Ordering::Relaxed) {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(item) = items.get(i) else {
                                return Ok(());
                            };
                            if let Err(e) = work(item) {
                                failed.store(true, Ordering::Relaxed);
                                return Err(e);
                            }
                            if i % 500 == 499 {
                                log::info!("client fetch: {} / {}", i + 1, items.len());
                            }
                        }
                        Ok(())
                    })
                    .expect("spawn download worker")
            })
            .collect();
        workers
            .into_iter()
            .try_for_each(|w| w.join().expect("download worker panicked"))
    })
}

fn complete_satisfies(marker: &Path, profile: AssetProfile) -> bool {
    match fs::read_to_string(marker).as_deref() {
        Ok("full") => true,
        Ok("minimal") => profile == AssetProfile::Minimal,
        _ => false,
    }
}

/// Downloads (or finds cached) client jar, libraries and assets for `version`.
/// The network is only touched when the cache is incomplete.
pub fn ensure_client(
    source: &dyn Source,
    cache_root: &Path,
    version: &str,
    profile: AssetProfile,
    platform: &Platform,
) -> Result<ClientFiles> {
    let client_dir = cache_root.join("client");
    let version_dir = client_dir.join(version);
    let marker = version_dir.join("fetch.complete");
    let version_json = version_dir.join("version.json");

    if !(complete_satisfies(&marker, profile) && version_json.is_file()) {
        let _lock = cache::Lock::acquire(cache_root)
            .with_context(|| format!("locking the cache {}", cache_root.display()))?;
        if !(complete_satisfies(&marker, profile) && version_json.is_file()) {
            fetch_all(source, &client_dir, version, profile, platform)?;
        }
    }

    let meta = VersionMeta::parse(&fs::read(&version_json)?)?;
    let client_jar = version_dir.join("client.jar");
    let mut classpath: Vec<PathBuf> = meta
        .libraries_for(platform)?
        .iter()
        .map(|a| client_dir.join("libraries").join(&a.path))
        .collect();
    classpath.push(client_jar.clone());
    Ok(ClientFiles {
        version: version.to_owned(),
        client_jar,
        classpath,
        natives_dir: version_dir.join("natives"),
        assets_dir: client_dir.join("assets"),
        asset_index_id: meta.asset_index.id.clone(),
        meta,
    })
}

fn fetch_all(
    source: &dyn Source,
    client_dir: &Path,
    version: &str,
    profile: AssetProfile,
    platform: &Platform,
) -> Result<()> {
    let version_dir = client_dir.join(version);
    let version_json = version_dir.join("version.json");
    if !version_json.is_file() {
        let manifest: VersionManifest = serde_json::from_slice(&source.get(VERSION_MANIFEST)?)
            .context("parsing the version manifest")?;
        let entry = manifest
            .versions
            .iter()
            .find(|v| v.id == version)
            .ok_or_else(|| anyhow!("version {version} is not in the Mojang version manifest"))?;
        let json = source.get(&entry.url)?;
        VersionMeta::parse(&json).context("parsing the version JSON")?;
        cache::write_atomic(&version_json, &json)?;
    }
    let meta = VersionMeta::parse(&fs::read(&version_json)?)?;

    let jar = &meta.downloads.client;
    download(source, &jar.url, &jar.sha1, &version_dir.join("client.jar"))?;

    let libraries = meta.libraries_for(platform)?;
    parallel(&libraries, |a| {
        download(
            source,
            &a.url,
            &a.sha1,
            &client_dir.join("libraries").join(&a.path),
        )
    })?;

    let assets = client_dir.join("assets");
    let index_path = assets
        .join("indexes")
        .join(format!("{}.json", meta.asset_index.id));
    download(
        source,
        &meta.asset_index.url,
        &meta.asset_index.sha1,
        &index_path,
    )?;
    let index: AssetIndex = serde_json::from_slice(&fs::read(&index_path)?)?;
    let mut hashes: BTreeMap<&str, u64> = BTreeMap::new();
    for (name, object) in &index.objects {
        if profile.includes(name) {
            hashes.insert(&object.hash, object.size);
        }
    }
    let objects: Vec<&str> = hashes.keys().copied().collect();
    parallel(&objects, |hash| {
        let prefix = &hash[..2];
        download(
            source,
            &format!("{ASSET_HOST}/{prefix}/{hash}"),
            hash,
            &assets.join("objects").join(prefix).join(hash),
        )
    })?;

    for dir in ["java", "jna", "lwjgl", "netty"] {
        fs::create_dir_all(version_dir.join("natives").join(dir))?;
    }
    cache::write_atomic(
        &version_dir.join("fetch.complete"),
        profile.name().as_bytes(),
    )?;
    Ok(())
}
