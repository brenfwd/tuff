//! `tuff gametest`: the headless game test server. It makes a fresh flat
//! creative world, runs the selected test instances, writes an optional
//! JUnit-like report and exits with the number of failed required tests.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use tuff_feature_builtin_vanilla_commands::gametest::{
    HeadlessError, HeadlessOptions, run_headless,
};

pub const SUBCOMMAND: &str = "gametest";
const DEFAULT_UNIVERSE: &str = "gametestserver";
const LEVEL_NAME: &str = "gametestworld";
const TEST_POSITION_RANGE: i64 = 14_999_992;
/// The exit status when no test matched or the server failed.
pub const ERROR_STATUS: i32 = 255;

/// What `tuff gametest` was asked for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options {
    pub universe: PathBuf,
    pub report: Option<PathBuf>,
    pub tests: Option<String>,
    pub verify: bool,
    pub repeat_count: i32,
    pub packs: Option<PathBuf>,
    pub help: bool,
    /// The first test's position; random (like the game) when `None`.
    pub origin: Option<[i32; 3]>,
}

pub fn usage() -> String {
    format!(
        "usage: {bin} {SUBCOMMAND} [--universe <dir>] [--report <file>] [--tests <selector>] \
         [--verify <true|false>] [--repeatCount <n>] [--packs <dir>] [--help]\n\
         \x20 --universe     where the test server world is created; an existing folder is replaced (default {DEFAULT_UNIVERSE})\n\
         \x20 --report       exports results in a JUnit-like XML report at the given path\n\
         \x20 --tests        which tests to run (namespaced ID selector using wildcards); empty runs all\n\
         \x20 --verify       runs the selected tests 100 times for each 90 degree rotation step\n\
         \x20 --repeatCount  runs each selected test this many times\n\
         \x20 --packs        a folder of data packs to include in the world",
        bin = env!("CARGO_PKG_NAME"),
    )
}

pub fn parse(raw: &[String]) -> Result<Options> {
    let mut options = Options {
        universe: PathBuf::from(DEFAULT_UNIVERSE),
        repeat_count: 1,
        ..Options::default()
    };
    let mut repeat_given = false;
    let mut iter = raw.iter();
    while let Some(arg) = iter.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let mut value = || -> Result<String> {
            inline
                .clone()
                .or_else(|| iter.next().cloned())
                .with_context(|| format!("{name} needs a value\n{}", usage()))
        };
        match name {
            "--help" => options.help = true,
            "--universe" => options.universe = PathBuf::from(value()?),
            "--report" => options.report = Some(PathBuf::from(value()?)),
            "--tests" => options.tests = Some(value()?),
            "--verify" => {
                options.verify = match value()?.as_str() {
                    "true" => true,
                    "false" => false,
                    other => bail!("--verify takes true or false, not {other:?}"),
                }
            }
            "--repeatCount" => {
                options.repeat_count = value()?.parse().context("--repeatCount takes a number")?;
                repeat_given = true;
            }
            "--packs" => options.packs = Some(PathBuf::from(value()?)),
            _ => {}
        }
    }
    if options.verify && options.tests.is_none() {
        bail!(
            "Please specify a test selection to run the verify option. For example: --verify true --tests example:test_something_*"
        );
    }
    if options.verify && repeat_given {
        tuff_core::log::info!("Flag --verify is true, the --repeatCount value will be ignored");
    }
    Ok(options)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Copies the folder packs (with a `pack.mcmeta`) and zip packs of
/// `source` into the world's data pack folder.
fn copy_packs(world: &Path, source: &Path) -> Result<()> {
    let target = world.join("datapacks");
    std::fs::create_dir_all(&target)?;
    if !source.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(source)? {
        let path = entry?.path();
        let Some(name) = path.file_name() else {
            continue;
        };
        if path.is_dir() {
            if path.join("pack.mcmeta").is_file() {
                copy_dir(&path, &target.join(name))?;
                tuff_core::log::info!("Included folder pack {}", name.to_string_lossy());
            }
        } else if path.extension().is_some_and(|e| e == "zip") {
            std::fs::copy(&path, target.join(name))?;
            tuff_core::log::info!("Included zip pack {}", name.to_string_lossy());
        }
    }
    Ok(())
}

fn random_origin() -> [i32; 3] {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(0);
    let bits = hasher.finish();
    let span = (2 * TEST_POSITION_RANGE + 1) as u64;
    let x = ((bits & 0xFFFF_FFFF) % span) as i64 - TEST_POSITION_RANGE;
    let z = ((bits >> 32) % span) as i64 - TEST_POSITION_RANGE;
    [x as i32, 4, z as i32]
}

/// Runs the game test server; the status to exit with (the failed
/// required test count, or [`ERROR_STATUS`]).
pub async fn run(options: Options) -> Result<i32> {
    if options.help {
        eprintln!("{}", usage());
        return Ok(0);
    }
    let cwd = std::env::current_dir().context("no current directory")?;
    tuff_core::log::info!(
        "Running GameTestMain with cwd '{}', universe path '{}'",
        cwd.display(),
        options.universe.display()
    );
    if options.universe.exists() {
        std::fs::remove_dir_all(&options.universe)
            .with_context(|| format!("cannot replace {}", options.universe.display()))?;
    }
    std::fs::create_dir_all(&options.universe)?;
    let world = options.universe.join(LEVEL_NAME);
    std::fs::create_dir_all(&world)?;
    if let Some(packs) = &options.packs {
        copy_packs(&world, packs)?;
    }
    std::fs::write(
        options.universe.join("server.properties"),
        format!(
            "level-name={LEVEL_NAME}\nlevel-type=minecraft:flat\ngamemode=creative\nonline-mode=false\n\
             spawn-protection=0\ngenerate-structures=false\n"
        ),
    )?;
    let server = tuff_core::Server::builder()
        .features(crate::vanilla_features::all())
        .instance_dir(options.universe.clone())
        .world_path(world)
        .online_mode(false)
        .whitelist(false)
        .serve(false)
        .seed(0)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let shared = handle
        .world_shared()
        .await
        .context("the game test server did not start")?;
    tuff_core::log::info!("Started game test server");
    let headless = HeadlessOptions {
        tests: options.tests.clone(),
        verify: options.verify,
        repeat_count: options.repeat_count,
        report: options.report.clone(),
    };
    let origin = options.origin.unwrap_or_else(random_origin);
    let outcome = run_headless(&shared, headless, origin).await;
    let _ = handle.shutdown().await;
    let _ = task.await;
    tuff_core::log::info!("Game test server shutting down");
    match outcome {
        Ok(summary) => Ok(summary.failed_required.len() as i32),
        Err(err @ HeadlessError::NoTests(_)) => {
            tuff_core::log::warn!("{err}");
            Ok(ERROR_STATUS)
        }
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn parses_the_game_flags() {
        let options = parse(&args(&[
            "--universe",
            "u",
            "--report=r.xml",
            "--tests",
            "example:*",
            "--repeatCount",
            "3",
            "--packs",
            "p",
        ]))
        .unwrap();
        assert_eq!(options.universe, PathBuf::from("u"));
        assert_eq!(options.report, Some(PathBuf::from("r.xml")));
        assert_eq!(options.tests.as_deref(), Some("example:*"));
        assert_eq!(options.repeat_count, 3);
        assert_eq!(options.packs, Some(PathBuf::from("p")));
        assert!(!options.verify);
    }

    #[tuff_core::watchdog]
    #[test]
    fn verify_needs_a_selection() {
        assert!(parse(&args(&["--verify", "true"])).is_err());
        assert!(
            parse(&args(&["--verify", "true", "--tests", "a"]))
                .unwrap()
                .verify
        );
        assert_eq!(
            parse(&[]).unwrap().universe,
            PathBuf::from(DEFAULT_UNIVERSE)
        );
    }
}
