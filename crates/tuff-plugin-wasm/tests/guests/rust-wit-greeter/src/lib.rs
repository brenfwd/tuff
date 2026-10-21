//! wit-greeter: the typed command tree example in Rust, on the SDK's
//! command builder; `../c-wit-greeter`, `../cpp-greeter` and
//! `../cpp-raw-greeter` are the same plugin in other languages.
//!
//! - `/wgreet` greets the sender
//! - `/wgreet <target>` greets a player, `/wgreet <target> <times>` 1 to 3 times
//! - `/wgreet sum <a> <b>` adds two integers
//! - `/wgreet volume <level>` takes a double from 0 to 1 (needs
//!   `wgreet.volume`) and suggests a few levels
//! - joins and leaves are broadcast

use tuff_plugin_sdk::command::{Ctx, arg, argument, command, literal, parser};
use tuff_plugin_sdk::events;
use tuff_plugin_sdk::types::{LogLevel, Player};
use tuff_plugin_sdk::{Plugin, host, plugin};

tuff_plugin_sdk::args! {
    struct Sum {
        a: i32,
        b: i32,
    }
}

struct Greeter;

fn greet(ctx: &Ctx, name: &str, times: i32) -> Result<i32, String> {
    for _ in 0..times {
        ctx.reply(format!("Hello, {name}!"))?;
    }
    Ok(times)
}

impl Plugin for Greeter {
    fn register() -> Result<(), String> {
        host::log(LogLevel::Info, "wit-greeter registering");
        command("wgreet")
            .executes(|ctx| greet(&ctx, &ctx.player()?.name, 1))
            .then(
                arg::<Player>("target")
                    .executes(|ctx| greet(&ctx, &ctx.get::<Player>("target")?.name, 1))
                    .then(
                        argument("times", parser::integer_in(1..=3)).executes(|ctx| {
                            let target: Player = ctx.get("target")?;
                            greet(&ctx, &target.name, ctx.get("times")?)
                        }),
                    ),
            )
            .then(literal("sum").with(|ctx, Sum { a, b }| {
                let sum = a.wrapping_add(b);
                ctx.reply(format!("{a} + {b} = {sum}"))?;
                Ok(sum)
            }))
            .then(
                literal("volume").permission("wgreet.volume").then(
                    argument("level", parser::double_in(0.0..=1.0))
                        .suggests(|_| vec!["0".into(), "0.5".into(), "1".into()])
                        .executes(|ctx| {
                            let level: f64 = ctx.get("level")?;
                            ctx.reply(format!("volume {}%", (level * 100.0) as i32))?;
                            Ok(1)
                        }),
                ),
            )
            .register()?;
        events::on::<events::ev::Join>(|p| {
            let _ = host::broadcast(&format!("{} joined", p.name));
        })?;
        events::on::<events::ev::Leave>(|p| {
            let _ = host::broadcast(&format!("{} left", p.name));
        })?;
        Ok(())
    }
}

plugin!(Greeter {
    id: "wgreet",
    version: "1.0.0",
    description: "Greets players through a typed command tree (Rust)",
});
