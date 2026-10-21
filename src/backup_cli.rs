//! `tuff backup ...`: backs up a world no server is running, a folder
//! world or a server file alike, and lists the backups. The running server
//! has `/tuff backup` instead. A thin wrapper over
//! `tuff_storage::backup`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tuff_core::datapack::store::OfflineWorld;
use tuff_storage::backup;
use tuff_storage::backup::{Plan, Tracker};

pub const SUBCOMMAND: &str = "backup";
const DEFAULT_COMPRESSION: u32 = 6;

pub fn usage() -> String {
    let bin = env!("CARGO_PKG_NAME");
    format!(
        "usage:
  {bin} {SUBCOMMAND} create <world> [--dir <folder>] [--compression <0-9>] [--include-caches]
  {bin} {SUBCOMMAND} list <world> [--dir <folder>]

<world> is a world folder (holding level.dat, e.g. ./world) or a server
file (e.g. ./world.mcserver). Backups go to <folder>, by default backups/
beside the world (the same folder a running server uses by default).

create   locks the world (a running server is refused; use /tuff backup
         create in it instead) and writes <name>-<yyyymmdd-hhmmss>.zip for
         a folder world, a copy of the file for a server file.
list     shows the backups of that world, newest first.
"
    )
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Create {
        world: PathBuf,
        dir: Option<PathBuf>,
        compression: u32,
        include_caches: bool,
    },
    List {
        world: PathBuf,
        dir: Option<PathBuf>,
    },
}

fn parse(args: &[String]) -> Result<Command> {
    let Some((action, rest)) = args.split_first() else {
        return Ok(Command::Help);
    };
    let mut positional = Vec::new();
    let mut dir = None;
    let mut compression = None;
    let mut include_caches = false;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dir" => {
                let value = iter.next().context("--dir needs a value")?;
                if dir.replace(PathBuf::from(value)).is_some() {
                    bail!("--dir given more than once");
                }
            }
            "--include-caches" if action == "create" => include_caches = true,
            "--compression" if action == "create" => {
                let value = iter.next().context("--compression needs a value")?;
                let level: u32 = value
                    .parse()
                    .ok()
                    .filter(|level| *level <= 9)
                    .context("--compression takes 0 to 9")?;
                if compression.replace(level).is_some() {
                    bail!("--compression given more than once");
                }
            }
            other if other.starts_with("--") => bail!("unknown option {other:?}\n{}", usage()),
            other => positional.push(PathBuf::from(other)),
        }
    }
    match (action.as_str(), positional.as_slice()) {
        ("help" | "--help" | "-h", _) => Ok(Command::Help),
        ("create", [world]) => Ok(Command::Create {
            world: world.clone(),
            dir,
            compression: compression.unwrap_or(DEFAULT_COMPRESSION),
            include_caches,
        }),
        ("list", [world]) => Ok(Command::List {
            world: world.clone(),
            dir,
        }),
        ("create" | "list", _) => bail!("{action} takes <world>\n{}", usage()),
        (other, _) => bail!("unknown {SUBCOMMAND} action {other:?}\n{}", usage()),
    }
}

fn level_of(world: &Path) -> String {
    let name = world
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(tuff_core::world::DEFAULT_LEVEL_NAME);
    backup::file_level(name)
}

fn default_dir(world: &Path, dir: Option<PathBuf>) -> PathBuf {
    dir.unwrap_or_else(|| {
        world
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("backups")
    })
}

/// Runs the arguments after `backup` and returns what to print.
pub fn run(args: &[String]) -> Result<String> {
    match parse(args)? {
        Command::Help => Ok(usage()),
        Command::Create {
            world,
            dir,
            compression,
            include_caches,
        } => {
            let opened = OfflineWorld::open(&world)
                .with_context(|| format!("cannot open the world {}", world.display()))?;
            let dir = default_dir(&world, dir);
            let plan = Plan {
                source: opened.backend().backup_source(),
                dir: dir.clone(),
                level: level_of(&world),
                compression,
                staged: false,
                include_caches,
            };
            let (tracker, _) = Tracker::new();
            let (name, size) = backup::run_offline(&plan, &tracker)
                .with_context(|| format!("cannot back up {}", world.display()))?;
            Ok(format!(
                "Created {} ({size} bytes)\n",
                dir.join(name).display()
            ))
        }
        Command::List { world, dir } => {
            let dir = default_dir(&world, dir);
            let list = backup::list(&dir, &level_of(&world))
                .with_context(|| format!("cannot list {}", dir.display()))?;
            if list.is_empty() {
                return Ok(format!("No backups in {}\n", dir.display()));
            }
            let mut out = String::new();
            for item in list {
                out.push_str(&format!("{}\t{} bytes\n", item.name, item.size));
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_storage::backend::WorldBackend;
    use tuff_storage::test_dir::TestDir;
    use tuff_storage::vanilla::VanillaBackend;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn parses_actions_and_options() {
        assert_eq!(parse(&[]).unwrap(), Command::Help);
        assert_eq!(
            parse(&args(&["create", "w", "--dir", "b", "--compression", "0"])).unwrap(),
            Command::Create {
                world: "w".into(),
                dir: Some("b".into()),
                compression: 0,
                include_caches: false
            }
        );
        assert_eq!(
            parse(&args(&["create", "w", "--include-caches"])).unwrap(),
            Command::Create {
                world: "w".into(),
                dir: None,
                compression: 6,
                include_caches: true
            }
        );
        assert!(parse(&args(&["list", "w", "--include-caches"])).is_err());
        assert!(parse(&args(&["create", "w", "--compression", "10"])).is_err());
        assert!(parse(&args(&["list", "w", "--compression", "1"])).is_err());
        assert!(parse(&args(&["create"])).is_err());
        assert!(parse(&args(&["restore", "w"])).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn creates_and_lists_backups_of_a_stopped_world() {
        let dir = TestDir::new();
        let world = dir.path().join("world");
        {
            let backend = VanillaBackend::new(&world);
            let _lock = backend.lock().unwrap();
            backend
                .documents()
                .write(
                    tuff_storage::document::Document::Level,
                    tuff_nbt::Compound::new().with("a", 1i32),
                )
                .unwrap();
        }
        let world_arg = world.display().to_string();
        let out = run(&args(&["create", &world_arg])).unwrap();
        assert!(out.starts_with("Created "), "{out}");
        let listed = run(&args(&["list", &world_arg])).unwrap();
        assert_eq!(listed.lines().count(), 1, "{listed}");
        assert!(listed.starts_with("world-"));

        let backend = VanillaBackend::new(&world);
        let _held = backend.lock().unwrap();
        assert!(
            run(&args(&["create", &world_arg])).is_err(),
            "a locked world is refused"
        );
    }
}
