//! `tuff replay <file.tuffrec> [--bind <addr>]...`: serves a packet
//! recording to viewers, who join in spectator mode (see `tuff_replay`).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};

pub const SUBCOMMAND: &str = "replay";
const DEFAULT_BIND: &str = "localhost:25565";

pub fn usage() -> String {
    let bin = env!("CARGO_PKG_NAME");
    format!(
        "usage:
  {bin} {SUBCOMMAND} <file.tuffrec> [--player <name>] [--bind <addr>]...

Serves the recording (offline mode) on {DEFAULT_BIND} or each --bind
address. The default is loopback only (127.0.0.1 and ::1): offline mode
accepts any username, so an explicit --bind lets anyone who can reach it join
and see the recording, including chat. For a global trace, --player <name>
follows that player and --player '*' follows the first player and adds every
other connection's chunks, blocks and entities for a free camera. Viewers join in spectator mode and control playback with
/replay play | pause | speed <x> | seek <seconds> | skip <+-seconds> | restart.
"
    )
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    file: PathBuf,
    binds: Vec<String>,
    player: Option<String>,
}

fn parse(args: &[String]) -> Result<Option<Args>> {
    let mut file = None;
    let mut binds = Vec::new();
    let mut player = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "help" | "--help" | "-h" => return Ok(None),
            "--bind" => binds.push(
                rest.next()
                    .with_context(|| format!("--bind needs an address\n{}", usage()))?
                    .clone(),
            ),
            "--player" => {
                player = Some(
                    rest.next()
                        .with_context(|| format!("--player needs a name\n{}", usage()))?
                        .clone(),
                )
            }
            flag if flag.starts_with("--") => bail!("unknown flag {flag}\n{}", usage()),
            path if file.is_none() => file = Some(PathBuf::from(path)),
            extra => bail!("unexpected argument {extra}\n{}", usage()),
        }
    }
    let Some(file) = file else {
        return Ok(None);
    };
    if binds.is_empty() {
        binds.push(DEFAULT_BIND.into());
    }
    Ok(Some(Args {
        file,
        binds,
        player,
    }))
}

/// Serves until interrupted.
pub async fn run(args: &[String]) -> Result<()> {
    let Some(args) = parse(args)? else {
        std::io::Write::write_all(&mut std::io::stdout(), usage().as_bytes())?;
        return Ok(());
    };
    let script = tuff_replay::script::load_for(
        &args.file,
        tuff_replay::MAX_RECORDING_BYTES,
        args.player.as_deref(),
    )
    .with_context(|| format!("cannot replay {}", args.file.display()))?;
    let (addrs, task) = tuff_replay::start(&args.binds, Arc::new(script)).await?;
    for addr in addrs {
        tuff_core::log::info!("replaying {} on {addr}", args.file.display());
    }
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = task => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn parses_the_file_and_binds() {
        assert_eq!(parse(&[]).unwrap(), None);
        assert_eq!(
            parse(&args(&["a.tuffrec"])).unwrap().unwrap().binds,
            vec![DEFAULT_BIND.to_string()]
        );
        assert_eq!(
            parse(&args(&[
                "a.tuffrec",
                "--bind",
                "[::]:1",
                "--bind",
                "0.0.0.0:2"
            ]))
            .unwrap()
            .unwrap(),
            Args {
                file: "a.tuffrec".into(),
                binds: vec!["[::]:1".into(), "0.0.0.0:2".into()],
                player: None,
            }
        );
        assert!(parse(&args(&["a", "--bind"])).is_err());
        assert!(parse(&args(&["a", "b"])).is_err());
        assert!(parse(&args(&["a", "--nope"])).is_err());
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn default_bind_is_loopback_only() {
        let binds = parse(&args(&["a.tuffrec"])).unwrap().unwrap().binds;
        let addrs = tokio::net::lookup_host(binds[0].as_str()).await.unwrap();
        let addrs: Vec<_> = addrs.collect();
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.ip().is_loopback()));
    }
}
