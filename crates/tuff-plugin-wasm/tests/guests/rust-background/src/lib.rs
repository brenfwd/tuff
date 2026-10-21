//! Conformance guest for background tasks on the Rust SDK: `/bg sum <text>`
//! sums the bytes of `text` in a task, `/bg bad` runs a task that fails and
//! `/bg trap` one that calls the host, which traps.

use tuff_plugin_sdk::background::{self, Error};
use tuff_plugin_sdk::prelude::*;

struct Guest;

fn sum(input: &[u8]) -> u64 {
    input.iter().map(|b| u64::from(*b)).sum()
}

impl Plugin for Guest {
    fn register() -> Result<(), String> {
        command("bg")
            .then(literal("sum").then(argument("text", parser::greedy()).executes_async(
                |ctx| async move {
                    let text: String = ctx.get("text").unwrap_or_default();
                    let line = match background::run("sum", text.as_bytes()).await {
                        Ok(out) => format!("sum {}", u64::from_le_bytes(out.try_into().unwrap())),
                        Err(error) => format!("error {error}"),
                    };
                    let _ = ctx.reply(line);
                },
            )))
            .then(literal("bad").executes_async(|ctx| async move {
                let line = match background::run("bad", b"").await {
                    Err(Error::Failed(why)) => format!("failed {why}"),
                    other => format!("unexpected {other:?}"),
                };
                let _ = ctx.reply(line);
            }))
            .then(literal("trap").executes_async(|ctx| async move {
                let line = match background::run("trap", b"").await {
                    Err(Error::Faulted(_)) => "faulted".to_owned(),
                    other => format!("unexpected {other:?}"),
                };
                let _ = ctx.reply(line);
            }))
            .register()
            .map_err(|e| format!("{e:?}"))
    }

    fn background(task: &str, input: &[u8]) -> Result<Vec<u8>, String> {
        match task {
            "sum" => Ok(sum(input).to_le_bytes().to_vec()),
            "bad" => Err("nope".into()),
            "trap" => {
                log(Level::Info, "this call traps");
                Ok(Vec::new())
            }
            _ => Err(format!("no task {task}")),
        }
    }
}

plugin!(Guest {
    id: "bgguest",
    version: "0.1.0",
    description: "Background task conformance guest",
});
