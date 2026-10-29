use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use tuff_nbt::{Compound, Compression, NamedTag, Tag};

use crate::ClientFiles;
use crate::meta::{Platform, expand_args};

pub const WIDTH: u32 = 960;
pub const HEIGHT: u32 = 540;
pub const HOOK_SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/java/TuffClientHook.java");

pub const AGENT_SOURCE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/java/agent/tuffagent/TuffAgent.java"
);

/// Compiles the instrumentation agent with the JDK next to `java` and packs
/// it into `<dir>/tuff-agent.jar`.
pub fn build_agent(java: &Path, dir: &Path) -> Result<PathBuf> {
    let bin = java.parent().context("java has no parent directory")?;
    let tool = |name: &str| {
        let p = bin.join(name);
        if p.exists() { p } else { PathBuf::from(name) }
    };
    let classes = dir.join("classes");
    fs::create_dir_all(&classes)?;
    let run = |program: PathBuf, args: Vec<String>| -> Result<()> {
        let out = std::process::Command::new(&program)
            .args(&args)
            .output()
            .with_context(|| format!("run {}", program.display()))?;
        anyhow::ensure!(
            out.status.success(),
            "{} failed: {}",
            program.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    };
    run(
        tool("javac"),
        vec![
            "-d".into(),
            classes.display().to_string(),
            AGENT_SOURCE.into(),
        ],
    )?;
    let manifest = dir.join("manifest.txt");
    fs::write(
        &manifest,
        "Premain-Class: tuffagent.TuffAgent\nCan-Retransform-Classes: true\n",
    )?;
    let jar = dir.join("tuff-agent.jar");
    run(
        tool("jar"),
        vec![
            "--create".into(),
            "--file".into(),
            jar.display().to_string(),
            "--manifest".into(),
            manifest.display().to_string(),
            "-C".into(),
            classes.display().to_string(),
            ".".into(),
        ],
    )?;
    Ok(jar)
}

/// Cache key for a compiled hook: the hook source, the classpath it compiles
/// against and the JDK launcher path.
pub fn hook_cache_key(source: &[u8], classpath: &[PathBuf], java: &Path) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(source);
    h.update([0]);
    for p in classpath {
        h.update(p.to_string_lossy().as_bytes());
        h.update([0]);
    }
    h.update(java.to_string_lossy().as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Compiles the hook against the game classpath into
/// `<cache>/<key>/`, reusing a previous compile when the key matches.
pub fn build_hook(java: &Path, classpath: &[PathBuf], cache: &Path) -> Result<PathBuf> {
    let source = fs::read(HOOK_SOURCE).context("read hook source")?;
    let dir = cache.join(hook_cache_key(&source, classpath, java));
    if dir.join("TuffClientHook.class").exists() {
        return Ok(dir);
    }
    let bin = java.parent().context("java has no parent directory")?;
    let javac = {
        let p = bin.join("javac");
        if p.exists() {
            p
        } else {
            PathBuf::from("javac")
        }
    };
    let tmp = cache.join(format!(
        "tmp-{}-{}",
        std::process::id(),
        RUN_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&tmp)?;
    let out = std::process::Command::new(&javac)
        .arg("-d")
        .arg(&tmp)
        .arg("-cp")
        .arg(std::env::join_paths(classpath).context("join classpath")?)
        .arg(HOOK_SOURCE)
        .output()
        .with_context(|| format!("run {}", javac.display()));
    let out = match out {
        Ok(out) => out,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    if !out.status.success() {
        let _ = fs::remove_dir_all(&tmp);
        anyhow::bail!("javac failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    if fs::rename(&tmp, &dir).is_err() {
        let _ = fs::remove_dir_all(&tmp);
        anyhow::ensure!(
            dir.join("TuffClientHook.class").exists(),
            "install compiled hook"
        );
    }
    Ok(dir)
}

/// Per-run artifacts directory `<root>/<test>-<pid>-<n>/`.
#[derive(Debug, Clone)]
pub struct RunDir {
    pub root: PathBuf,
}

static RUN_COUNTER: AtomicUsize = AtomicUsize::new(0);

impl RunDir {
    pub fn create(base: &Path, test: &str) -> Result<Self> {
        let safe: String = test
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let n = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = base.join(format!("{safe}-{}-{n}", std::process::id()));
        fs::create_dir_all(root.join("game"))?;
        Ok(Self { root })
    }

    pub fn game_dir(&self) -> PathBuf {
        self.root.join("game")
    }

    pub fn quickplay_path(&self) -> PathBuf {
        self.root.join("quickplay.json")
    }

    pub fn client_log(&self) -> PathBuf {
        self.root.join("client.log")
    }
}

pub fn default_run_base() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/client-harness")
}

pub fn options_txt(data_version: i32) -> String {
    options_txt_with_view(data_version, 6, 5)
}

pub fn parity_view_distance() -> Option<i32> {
    std::env::var("PARITY_VIEW_DISTANCE").ok()?.parse().ok()
}

pub fn options_txt_with_view(data_version: i32, render: i32, simulation: i32) -> String {
    let lines = [
        format!("version:{data_version}"),
        "onboardAccessibility:false".into(),
        "skipMultiplayerWarning:true".into(),
        "joinedFirstServer:true".into(),
        "tutorialStep:none".into(),
        "pauseOnLostFocus:false".into(),
        format!("renderDistance:{render}"),
        format!("simulationDistance:{simulation}"),
        "maxFps:60".into(),
        "enableVsync:false".into(),
        "fullscreen:false".into(),
        "narrator:0".into(),
        "soundCategory_master:0.0".into(),
        "realmsNotifications:false".into(),
        "autoJump:false".into(),
        "chatLinksPrompt:false".into(),
        "lang:en_us".into(),
        "guiScale:2".into(),
    ];
    lines.join("\n") + "\n"
}

/// Uncompressed NBT with one entry for `address` that auto-accepts the
/// server resource pack.
pub fn servers_dat(address: &str) -> Result<Vec<u8>> {
    let entry = Compound::new()
        .with("name", "tuff test")
        .with("ip", address)
        .with("acceptTextures", Tag::Byte(1));
    let root = Compound::new().with("servers", vec![entry]);
    Ok(NamedTag::new("", Tag::Compound(root)).to_bytes(Compression::None)?)
}

pub fn write_game_dir(run: &RunDir, address: &str, data_version: i32) -> Result<()> {
    let game = run.game_dir();
    fs::create_dir_all(&game)?;
    let options = match parity_view_distance() {
        Some(view) => options_txt_with_view(data_version, view, view),
        None => options_txt(data_version),
    };
    fs::write(game.join("options.txt"), options)?;
    fs::write(game.join("servers.dat"), servers_dat(address)?)?;
    Ok(())
}

pub fn uuid_no_dashes(uuid: u128) -> String {
    format!("{uuid:032x}")
}

#[derive(Debug)]
pub struct LaunchSpec<'a> {
    pub java: &'a Path,
    pub files: &'a ClientFiles,
    pub run: &'a RunDir,
    pub username: &'a str,
    /// `host:port` exactly as written to servers.dat and quick play.
    pub address: &'a str,
    pub platform: &'a Platform,
    /// Extra `-D` properties placed before the hook source (control port and token).
    pub hook_args: &'a [String],
    /// Directory of the precompiled hook; `None` launches the source in memory.
    pub hook_dir: Option<&'a Path>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

fn path_str(p: &Path) -> String {
    p.display().to_string()
}

pub fn build_command(spec: &LaunchSpec) -> Result<LaunchCommand> {
    let files = spec.files;
    let natives = &files.natives_dir;
    let mut entries: Vec<PathBuf> = spec.hook_dir.map(Path::to_path_buf).into_iter().collect();
    entries.extend(files.classpath.iter().cloned());
    let classpath = std::env::join_paths(&entries).context("join classpath")?;
    let uuid = uuid_no_dashes(tuff_core::auth::offline_uuid(spec.username));
    let vars: BTreeMap<String, String> = [
        ("auth_player_name", spec.username.to_owned()),
        ("auth_uuid", uuid.clone()),
        ("auth_access_token", "0".to_owned()),
        ("user_type", "legacy".to_owned()),
        ("clientid", "0".to_owned()),
        ("auth_xuid", "0".to_owned()),
        ("version_name", files.version.clone()),
        ("version_type", "release".to_owned()),
        ("game_directory", path_str(&spec.run.game_dir())),
        ("assets_root", path_str(&files.assets_dir)),
        ("assets_index_name", files.asset_index_id.clone()),
        ("resolution_width", WIDTH.to_string()),
        ("resolution_height", HEIGHT.to_string()),
        ("quickPlayPath", path_str(&spec.run.quickplay_path())),
        ("quickPlayMultiplayer", spec.address.to_owned()),
        ("launcher_name", "tuff-client-harness".to_owned()),
        ("launcher_version", env!("CARGO_PKG_VERSION").to_owned()),
        ("classpath", classpath.to_string_lossy().into_owned()),
        (
            "classpath_separator",
            if cfg!(windows) { ";" } else { ":" }.to_owned(),
        ),
        ("natives_directory", path_str(natives)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();

    let mut args: Vec<String> = [
        "-Xms512M",
        "-Xmx4G",
        "-XX:+UseZGC",
        "-XX:MaxDirectMemorySize=1G",
        "-XX:+UseCompactObjectHeaders",
        "-XX:+UseStringDeduplication",
    ]
    .map(String::from)
    .into();
    args.extend(expand_args(
        &files.meta.arguments.jvm,
        spec.platform,
        &vars,
    )?);
    args.push(format!(
        "-Djava.library.path={}",
        natives.join("java").display()
    ));
    args.push(format!("-Djna.tmpdir={}", natives.join("jna").display()));
    args.push(format!(
        "-Dorg.lwjgl.system.SharedLibraryExtractPath={}",
        natives.join("lwjgl").display()
    ));
    args.push(format!(
        "-Dio.netty.native.workdir={}",
        natives.join("netty").display()
    ));
    args.push(format!(
        "-Dtuff.harness.mainClass={}",
        files.meta.main_class
    ));
    args.extend(spec.hook_args.iter().cloned());
    args.push(if spec.hook_dir.is_some() {
        "TuffClientHook".to_owned()
    } else {
        HOOK_SOURCE.to_owned()
    });
    args.extend(expand_args(
        &files.meta.arguments.game,
        spec.platform,
        &vars,
    )?);
    Ok(LaunchCommand {
        program: spec.java.to_path_buf(),
        args,
    })
}
