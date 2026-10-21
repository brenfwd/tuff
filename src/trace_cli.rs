//! `tuff trace dump <file> [--no-chat]`: prints a packet recording made by
//! `/tuff tracing` as text lines, one packet per line.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tuff_core::trace::dump::{Options, render};

pub const SUBCOMMAND: &str = "trace";

pub fn usage() -> String {
    let bin = env!("CARGO_PKG_NAME");
    format!(
        "usage:
  {bin} {SUBCOMMAND} dump <file.tuffrec> [--no-chat]

dump   prints every record as `<unix ns> <state> <sb|cb> <text>`;
       --no-chat leaves out chat packets.
"
    )
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Dump { file: PathBuf, no_chat: bool },
}

fn parse(args: &[String]) -> Result<Command> {
    match args.first().map(String::as_str) {
        None | Some("help" | "--help" | "-h") => Ok(Command::Help),
        Some("dump") => {
            let mut file = None;
            let mut no_chat = false;
            for arg in &args[1..] {
                match arg.as_str() {
                    "--no-chat" => no_chat = true,
                    flag if flag.starts_with("--") => bail!("unknown flag {flag}\n{}", usage()),
                    path if file.is_none() => file = Some(PathBuf::from(path)),
                    extra => bail!("unexpected argument {extra}\n{}", usage()),
                }
            }
            let file = file.with_context(|| format!("dump needs a file\n{}", usage()))?;
            Ok(Command::Dump { file, no_chat })
        }
        Some(other) => bail!("unknown trace command {other}\n{}", usage()),
    }
}

/// Runs the subcommand, writing its output to `out`.
pub fn run(args: &[String], out: &mut impl Write) -> Result<()> {
    match parse(args)? {
        Command::Help => out.write_all(usage().as_bytes())?,
        Command::Dump { file, no_chat } => {
            let input = std::fs::File::open(&file)
                .with_context(|| format!("cannot open {}", file.display()))?;
            let mut failed = None;
            render(
                std::io::BufReader::new(input),
                Options { no_chat },
                |line| {
                    if failed.is_none()
                        && let Err(err) = out.write_all(line.as_bytes())
                    {
                        failed = Some(err);
                    }
                },
            )
            .with_context(|| format!("cannot read {}", file.display()))?;
            if let Some(err) = failed {
                return Err(err.into());
            }
        }
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
    fn parses_dump_and_flags() {
        assert_eq!(parse(&[]).unwrap(), Command::Help);
        assert_eq!(
            parse(&args(&["dump", "a.tuffrec", "--no-chat"])).unwrap(),
            Command::Dump {
                file: "a.tuffrec".into(),
                no_chat: true
            }
        );
        assert!(parse(&args(&["dump"])).is_err());
        assert!(parse(&args(&["dump", "a", "b"])).is_err());
        assert!(parse(&args(&["dump", "a", "--bogus"])).is_err());
        assert!(parse(&args(&["play"])).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn dump_refuses_a_file_that_is_not_a_recording() {
        let path = std::env::temp_dir().join(format!("tuff-trace-cli-{}.txt", std::process::id()));
        std::fs::write(&path, b"hello").unwrap();
        let mut out = Vec::new();
        let err = run(&args(&["dump", path.to_str().unwrap()]), &mut out).unwrap_err();
        assert!(
            format!("{err:#}").contains("not a tuff recording"),
            "{err:#}"
        );
        let _ = std::fs::remove_file(path);
    }
}
