//! `tuff datapack ...`: adds, lists and removes the data packs of a world no
//! server is running, a folder world or a server file alike. A thin wrapper
//! over `tuff_core::datapack::store`, which does all the checking.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tuff_core::datapack::Compatibility;
use tuff_core::datapack::store::{self, Kind, OfflineWorld, Selection, Upload};

pub const SUBCOMMAND: &str = "datapack";

pub fn usage() -> String {
    let bin = env!("CARGO_PKG_NAME");
    format!(
        "usage:
  {bin} {SUBCOMMAND} add <world> <pack.zip|folder> [--name <name>] [--enable] [--replace]
  {bin} {SUBCOMMAND} list <world>
  {bin} {SUBCOMMAND} remove <world> <name>

<world> is a world folder (holding level.dat, e.g. ./world) or a server
file (e.g. ./world.mcserver). The server must be stopped: the world is
locked while the command runs, and a running server is refused.

add      checks the pack (pack.mcmeta at its root, size and file limits, no
         paths leaving the pack) and stores it under datapacks/: a zip as
         datapacks/<name>.zip, a folder as datapacks/<name>/. The name
         defaults to the file or folder name. --replace overwrites a pack
         of the same name; --enable also puts it in the world's saved
         selection (needed only when the world had disabled it; new packs
         load on the next start by themselves).
list     shows each pack's id, whether the saved selection enables it,
         and its description.
remove   deletes a pack by name or id (file/<name>).

Without this command, a folder world takes packs copied into
<world>/datapacks/. A server file keeps them in its sqlar table under
extra/datapacks/, which the sqlite3 shell can update while the server is
stopped:
  mkdir -p stage/extra/datapacks && cp pack.zip stage/extra/datapacks/
  sqlite3 world.mcserver -A -u -C stage extra/datapacks/pack.zip
"
    )
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Add {
        world: PathBuf,
        source: PathBuf,
        name: Option<String>,
        enable: bool,
        replace: bool,
    },
    List {
        world: PathBuf,
    },
    Remove {
        world: PathBuf,
        name: String,
    },
}

fn parse(args: &[String]) -> Result<Command> {
    let Some((action, rest)) = args.split_first() else {
        return Ok(Command::Help);
    };
    let mut positional = Vec::new();
    let mut name = None;
    let mut enable = false;
    let mut replace = false;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--enable" if action == "add" => enable = true,
            "--replace" if action == "add" => replace = true,
            "--name" if action == "add" => {
                let value = iter.next().context("--name needs a value")?;
                if name.replace(value.clone()).is_some() {
                    bail!("--name given more than once");
                }
            }
            other if action == "add" && other.starts_with("--name=") => {
                if name.replace(other["--name=".len()..].to_owned()).is_some() {
                    bail!("--name given more than once");
                }
            }
            other if other.starts_with("--") => bail!("unknown option {other:?}\n{}", usage()),
            other => positional.push(other.to_owned()),
        }
    }
    let wrong =
        |expected: &str| -> Result<Command> { bail!("{action} takes {expected}\n{}", usage()) };
    match (action.as_str(), positional.as_slice()) {
        ("help" | "--help" | "-h", _) => Ok(Command::Help),
        ("add", [world, source]) => Ok(Command::Add {
            world: world.into(),
            source: source.into(),
            name,
            enable,
            replace,
        }),
        ("add", _) => wrong("<world> <pack.zip|folder>"),
        ("list", [world]) => Ok(Command::List {
            world: world.into(),
        }),
        ("list", _) => wrong("<world>"),
        ("remove", [world, name]) => Ok(Command::Remove {
            world: world.into(),
            name: name.clone(),
        }),
        ("remove", _) => wrong("<world> <name>"),
        (other, _) => bail!("unknown {SUBCOMMAND} action {other:?}\n{}", usage()),
    }
}

fn open(world: &Path) -> Result<OfflineWorld> {
    OfflineWorld::open(world).with_context(|| format!("cannot open the world {}", world.display()))
}

fn compatibility_note(compatibility: Compatibility) -> &'static str {
    match compatibility {
        Compatibility::Compatible => "",
        Compatibility::TooOld => " (made for an older game version)",
        Compatibility::TooNew => " (made for a newer game version)",
        Compatibility::Unknown => " (declares no readable pack format)",
    }
}

/// Runs the arguments after `datapack` and returns what to print.
pub fn run(args: &[String]) -> Result<String> {
    match parse(args)? {
        Command::Help => Ok(usage()),
        Command::Add {
            world,
            source,
            name,
            enable,
            replace,
        } => {
            let name = match name {
                Some(name) => name,
                None => store::suggested_name(&source).with_context(|| {
                    format!("cannot name a pack after {}; use --name", source.display())
                })?,
            };
            let upload = Upload::read_path(&source)
                .with_context(|| format!("cannot read the pack {}", source.display()))?;
            let world = open(&world)?;
            let backend = world.backend();
            let added = store::add(backend, &name, &upload, replace)?;
            let id = added.id();
            let mut out = format!(
                "{} {id}: {} file(s), {} byte(s){}\n",
                if added.replaced { "replaced" } else { "added" },
                added.files,
                added.bytes,
                compatibility_note(added.meta.compatibility()),
            );
            if enable {
                if store::enable(backend, &id)? {
                    out.push_str("enabled it in the world's saved selection\n");
                } else {
                    out.push_str("the world has no saved selection yet; it loads on first start\n");
                }
            } else {
                let disabled = store::saved_selection(backend)?
                    .is_some_and(|config| config.disabled.contains(&id));
                if disabled {
                    out.push_str(&format!(
                        "the world's saved selection disables it: add with --enable, or run \
                         /datapack enable \"{id}\" on the server\n"
                    ));
                } else {
                    out.push_str("it loads on the next server start\n");
                }
            }
            Ok(out)
        }
        Command::List { world } => {
            let world = open(&world)?;
            let entries = store::list(world.backend())?;
            if entries.is_empty() {
                return Ok("no data packs in the world\n".to_owned());
            }
            let mut out = String::new();
            for entry in entries {
                let selection = match entry.selection {
                    Selection::Enabled => "enabled",
                    Selection::Disabled => "disabled",
                    Selection::New => "new",
                };
                let kind = match entry.kind {
                    Kind::Directory => "folder",
                    Kind::Zip => "zip",
                };
                let about = match &entry.meta {
                    Ok(meta) => format!(
                        "{}{}",
                        meta.description.plain(),
                        compatibility_note(meta.compatibility())
                    ),
                    Err(err) => format!("not loadable: {err}"),
                };
                out.push_str(&format!("{}\t{selection}\t{kind}\t{about}\n", entry.id()));
            }
            Ok(out)
        }
        Command::Remove { world, name } => {
            let world = open(&world)?;
            let removed = store::remove(world.backend(), &name)?;
            Ok(format!(
                "removed {} ({} file(s))\n",
                removed.id(),
                removed.files()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::datapack::meta::Format;
    use tuff_datapack::zip;
    use tuff_storage::backend::WorldBackend;
    use tuff_storage::document::Document;
    use tuff_storage::sqlite::SqliteBackend;
    use tuff_storage::test_dir::TestDir;
    use tuff_storage::vanilla::VanillaBackend;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    fn cli(list: &[&str]) -> Result<String> {
        run(&args(list))
    }

    fn pack_json(description: &str) -> String {
        let current = Format::current().major;
        format!(
            r#"{{"pack":{{"description":"{description}","min_format":{current},"max_format":{current}}}}}"#
        )
    }

    fn level(backend: &dyn WorldBackend, disabled: &[&str]) {
        use tuff_nbt::{Compound, Tag};
        let list = |names: &[&str]| Tag::List(names.iter().map(|n| Tag::from(*n)).collect());
        backend
            .documents()
            .write(
                Document::Level,
                Compound::new().with(
                    "Data",
                    Compound::new().with(
                        "DataPacks",
                        Compound::new()
                            .with("Enabled", list(&["vanilla"]))
                            .with("Disabled", list(disabled)),
                    ),
                ),
            )
            .unwrap();
    }

    fn sources(dir: &TestDir) -> (String, String) {
        let archive = dir.path().join("My Pack.zip");
        std::fs::write(
            &archive,
            zip::write(&[("pack.mcmeta", pack_json("zipped").as_bytes())]),
        )
        .unwrap();
        let folder = dir.path().join("folder");
        std::fs::create_dir_all(folder.join("data/a/function")).unwrap();
        std::fs::write(folder.join("pack.mcmeta"), pack_json("a folder")).unwrap();
        std::fs::write(folder.join("data/a/function/f.mcfunction"), "say hi").unwrap();
        (archive.display().to_string(), folder.display().to_string())
    }

    fn round_trip(world: &str, dir: &TestDir) {
        let (archive, folder) = sources(dir);
        let out = cli(&["add", world, &archive]).unwrap();
        assert!(
            out.starts_with("added file/My_Pack.zip: 1 file(s)"),
            "{out}"
        );
        assert!(out.contains("saved selection disables it"), "{out}");
        let out = cli(&["add", world, &folder, "--name", "tools"]).unwrap();
        assert!(out.contains("loads on the next server start"), "{out}");
        cli(&["remove", world, "tools"]).unwrap();
        let out = cli(&["add", world, &folder, "--name", "tools", "--enable"]).unwrap();
        assert!(out.starts_with("added file/tools: 2 file(s)"), "{out}");
        assert!(out.contains("enabled it"), "{out}");
        let err = cli(&["add", world, &folder, "--name=tools"]).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        let out = cli(&["add", world, &folder, "--name=tools", "--replace"]).unwrap();
        assert!(out.starts_with("replaced file/tools"), "{out}");
        assert_eq!(
            cli(&["list", world]).unwrap(),
            "file/My_Pack.zip\tdisabled\tzip\tzipped\nfile/tools\tenabled\tfolder\ta folder\n"
        );
        assert_eq!(
            cli(&["remove", world, "file/tools"]).unwrap(),
            "removed file/tools (2 file(s))\n"
        );
        assert_eq!(
            cli(&["remove", world, "My_Pack.zip"]).unwrap(),
            "removed file/My_Pack.zip (1 file(s))\n"
        );
        assert_eq!(
            cli(&["list", world]).unwrap(),
            "no data packs in the world\n"
        );
        let err = cli(&["remove", world, "tools"]).unwrap_err();
        assert!(err.to_string().contains("no pack file/tools"), "{err}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn manages_packs_of_a_folder_world() {
        let dir = TestDir::new();
        let world = dir.path().join("world");
        level(&VanillaBackend::new(&world), &["file/My_Pack.zip"]);
        round_trip(&world.display().to_string(), &dir);
    }

    #[tuff_core::watchdog]
    #[test]
    fn manages_packs_of_a_server_file() {
        let dir = TestDir::new();
        let file = dir.path().join("world.mcserver");
        level(&SqliteBackend::open(&file).unwrap(), &["file/My_Pack.zip"]);
        round_trip(&file.display().to_string(), &dir);
    }

    #[tuff_core::watchdog]
    #[test]
    fn refuses_a_running_world_and_bad_packs() {
        let dir = TestDir::new();
        let (archive, _) = sources(&dir);
        let file = dir.path().join("world.mcserver");
        let server = SqliteBackend::open(&file).unwrap();
        let held = server.lock().unwrap();
        let world = file.display().to_string();
        let err = format!("{:#}", cli(&["add", &world, &archive]).unwrap_err());
        assert!(err.contains("stop the server first"), "{err}");
        drop(held);
        drop(server);

        let bad = dir.path().join("bad.zip");
        std::fs::write(&bad, b"not a zip").unwrap();
        let err = format!(
            "{:#}",
            cli(&["add", &world, &bad.display().to_string()]).unwrap_err()
        );
        assert!(err.contains("unreadable zip"), "{err}");
        let err = cli(&["add", &world, &archive, "--name", "../x"]).unwrap_err();
        assert!(err.to_string().contains("invalid pack name"), "{err}");
        assert_eq!(
            cli(&["list", &world]).unwrap(),
            "no data packs in the world\n"
        );
        let err = format!("{:#}", cli(&["list", "/nonexistent/world"]).unwrap_err());
        assert!(err.contains("no world at"), "{err}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn parses_actions_and_options() {
        assert_eq!(parse(&[]).unwrap(), Command::Help);
        assert_eq!(parse(&args(&["--help"])).unwrap(), Command::Help);
        assert!(
            cli(&["help"])
                .unwrap()
                .contains("sqlite3 world.mcserver -A -u")
        );
        assert_eq!(
            parse(&args(&["add", "w", "p.zip", "--enable", "--name=n"])).unwrap(),
            Command::Add {
                world: "w".into(),
                source: "p.zip".into(),
                name: Some("n".into()),
                enable: true,
                replace: false,
            }
        );
        for bad in [
            &["add", "w"][..],
            &["add", "w", "p", "extra"],
            &["add", "w", "p", "--name"],
            &["add", "w", "p", "--name=a", "--name", "b"],
            &["list"],
            &["list", "w", "--enable"],
            &["remove", "w"],
            &["frobnicate", "w"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
