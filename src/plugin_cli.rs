//! `tuff plugin new` scaffolds a plugin project (see [`crate::plugin_new`]).
//! `tuff plugin grants|grant|revoke ...`: edits runtime plugin grants in a
//! server file no server is running on, so an owner can grant before the
//! world is first generated. It shares its validation and storage with
//! `/tuff plugin grant|revoke|grants` through `tuff_plugin_wasm::grants_edit`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tuff_core::world::DEFAULT_LEVEL_NAME;
use tuff_plugin_wasm::DEFAULT_DIRECTORY;
use tuff_plugin_wasm::grants_edit::{GrantChange, change_offline, report_offline};
use tuff_storage::backend::WorldBackend;
use tuff_storage::config::ConfigStore;
use tuff_storage::sqlite::{self, SqliteBackend};
use tuff_storage::vanilla::config::VanillaConfig;

pub const SUBCOMMAND: &str = "plugin";

pub fn usage() -> String {
    let bin = env!("CARGO_PKG_NAME");
    format!(
        "usage:
  {bin} {SUBCOMMAND} grants <id> [OPTIONS]
  {bin} {SUBCOMMAND} grant <id> <cap> <entry|all-requested> [OPTIONS]
  {bin} {SUBCOMMAND} revoke <id> <cap> <entry|all> [OPTIONS]
  {bin} {SUBCOMMAND} new <id> [--lang rust|cpp] [--dir <dir>] [--sdk <dir>]

OPTIONS
  --server_file <file>   the server file (default ./{DEFAULT_LEVEL_NAME}.{ext}; created if missing)
  --plugins <dir>        where the plugin modules are (default ./{DEFAULT_DIRECTORY})

Same arguments as /tuff plugin in game. Grants live in the SQLite server
file, or in tuff.db inside a folder world. A running server holds the
file; use /tuff plugin in it instead. all-requested never grants private
networks or top routes; grant those by naming them.
",
        ext = sqlite::EXTENSION,
    )
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Grants { id: String },
    Change { id: String, change: GrantChange },
}

#[derive(Debug, PartialEq, Eq)]
struct Parsed {
    command: Command,
    server_file: Option<PathBuf>,
    plugins: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Parsed> {
    let Some((action, rest)) = args.split_first() else {
        return Ok(Parsed {
            command: Command::Help,
            server_file: None,
            plugins: None,
        });
    };
    let (mut server_file, mut plugins) = (None, None);
    let mut positional = Vec::new();
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        let slot = match arg.as_str() {
            "--server_file" => &mut server_file,
            "--plugins" => &mut plugins,
            other if other.starts_with("--") => bail!("unknown option {other:?}\n{}", usage()),
            other => {
                positional.push(other.to_owned());
                continue;
            }
        };
        let value = iter
            .next()
            .with_context(|| format!("{arg} needs a value"))?;
        if slot.replace(PathBuf::from(value)).is_some() {
            bail!("{arg} given more than once");
        }
    }
    let command = match (action.as_str(), positional.as_slice()) {
        ("help" | "--help" | "-h", _) => Command::Help,
        ("grants", [id]) => Command::Grants { id: id.clone() },
        ("grant" | "revoke", [id, cap, entry @ ..]) if !entry.is_empty() => {
            let (cap, entry) = (cap.clone(), entry.join(" "));
            Command::Change {
                id: id.clone(),
                change: match action.as_str() {
                    "grant" => GrantChange::Grant { cap, entry },
                    _ => GrantChange::Revoke { cap, entry },
                },
            }
        }
        ("grants" | "grant" | "revoke", _) => bail!("wrong arguments for {action}\n{}", usage()),
        (other, _) => bail!("unknown {SUBCOMMAND} action {other:?}\n{}", usage()),
    };
    Ok(Parsed {
        command,
        server_file,
        plugins,
    })
}

type Opened = (Arc<dyn ConfigStore>, Option<SqliteBackend>);

fn open(file: &Path) -> Result<Opened> {
    if file.is_dir() {
        let store: Arc<dyn ConfigStore> = Arc::new(VanillaConfig::new(file));
        return Ok((store, None));
    }
    let backend = SqliteBackend::open(file).with_context(|| {
        format!(
            "cannot open {} (is a server running on it?)",
            file.display()
        )
    })?;
    let store = backend
        .config()
        .context("this storage backend has no configuration store")?;
    Ok((store, Some(backend)))
}

/// Runs the arguments after `plugin` and returns what to print.
pub fn run(args: &[String]) -> Result<String> {
    if args.first().map(String::as_str) == Some(crate::plugin_new::ACTION) {
        return crate::plugin_new::run(&args[1..]);
    }
    let parsed = parse(args)?;
    if parsed.command == Command::Help {
        return Ok(usage());
    }
    let file = parsed
        .server_file
        .unwrap_or_else(|| PathBuf::from(format!("{DEFAULT_LEVEL_NAME}.{}", sqlite::EXTENSION)));
    let plugins = parsed
        .plugins
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DIRECTORY));
    let (store, backend) = open(&file)?;
    let result = match parsed.command {
        Command::Change { id, change } => {
            change_offline(&*store, &plugins, &id, change).map(|done| format!("{done}\n"))
        }
        Command::Grants { id } => report_offline(&*store, &plugins, &id).map(|report| {
            let mut out = format!("{id} requests {} capabilities\n", report.requested.len());
            for (entry, reason, granted) in &report.requested {
                let reason = match reason.is_empty() {
                    true => String::new(),
                    false => format!(" ({reason})"),
                };
                let state = if *granted { "granted" } else { "NOT GRANTED" };
                out.push_str(&format!("  {state}: {entry}{reason}\n"));
            }
            for entry in &report.extra {
                out.push_str(&format!("  granted, not requested: {entry}\n"));
            }
            for note in &report.notes {
                out.push_str(&format!("  note: {note}\n"));
            }
            out
        }),
        Command::Help => unreachable!("handled above"),
    };
    drop(store);
    if let Some(backend) = backend {
        backend.close();
    }
    result.map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_args(args: &[&str]) -> Result<String> {
        run(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[tuff_core::watchdog]
    #[test]
    fn arguments_parse_like_the_in_game_command() {
        let args: Vec<String> = [
            "grant",
            "relay",
            "route",
            "/dynmap",
            "top",
            "--plugins",
            "p",
        ]
        .map(String::from)
        .to_vec();
        let parsed = parse(&args).unwrap();
        assert_eq!(parsed.plugins, Some(PathBuf::from("p")));
        assert_eq!(
            parsed.command,
            Command::Change {
                id: "relay".into(),
                change: GrantChange::Grant {
                    cap: "route".into(),
                    entry: "/dynmap top".into()
                }
            }
        );
        assert!(parse(&["grant".into(), "relay".into()]).is_err());
        assert!(parse(&["nope".into()]).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_folder_world_is_accepted() {
        let dir = std::env::temp_dir().join(format!("tuff-plugin-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = run_args(&[
            "grants",
            "relay",
            "--server_file",
            dir.to_str().unwrap(),
            "--plugins",
            dir.to_str().unwrap(),
        ])
        .unwrap_err()
        .to_string();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(err.contains("no plugin relay"), "{err}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_unknown_plugin_is_refused() {
        let dir = std::env::temp_dir().join(format!("tuff-plugin-cli-u-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("w.mcserver");
        let err = run_args(&[
            "grant",
            "ghost",
            "http",
            "all-requested",
            "--server_file",
            file.to_str().unwrap(),
            "--plugins",
            dir.to_str().unwrap(),
        ])
        .unwrap_err()
        .to_string();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(err.contains("no plugin ghost"), "{err}");
    }
}
