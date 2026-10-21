// SPDX-License-Identifier: MIT
//! Calls the optional greeter example plugin through its typed interface.
//!
//! - `/greeted greet <name>` asks the greeter to greet `<name>`, and falls
//!   back to a plain greeting when the greeter is not installed
//! - `/greeted answer` repeats the last answer
//! - `/greeted last` reports the last `counted` event the greeter sent
//! - `/greeted greeter` reports the last plugin-change for the greeter

use std::cell::RefCell;

use tuff_plugin_sdk::prelude::*;
use tuff_plugin_sdk::wire::Error;

#[path = "../../../crates/tuff-plugin-wasm/tests/guests/rust-messaging-provider/src/api.rs"]
#[rustfmt::skip]
#[allow(dead_code)]
mod api;

use api::{counted, greet};

thread_local! {
    static LAST: RefCell<Option<(String, u32)>> = const { RefCell::new(None) };
    static ANSWER: RefCell<String> = const { RefCell::new(String::new()) };
    static GREETER_UP: RefCell<Option<bool>> = const { RefCell::new(None) };
}

struct Counts;

impl counted::Serve for Counts {
    fn counted(&mut self, _from: &str, name: String, total: u32) {
        LAST.with(|last| *last.borrow_mut() = Some((name, total)));
    }
}

struct Client;

async fn greet_through_greeter(ctx: Ctx, name: String) {
    let line = match greet::Greet::any().greet(name).await {
        Ok(line) => line,
        Err(Error::App(greet::GreetError::EmptyName)) => "Say who to greet".to_owned(),
        Err(Error::Call(CallError::NoProvider)) => {
            "The greeter plugin is not installed, so: hello there".to_owned()
        }
        Err(other) => format!("The greeter failed: {other:?}"),
    };
    ANSWER.with(|answer| answer.replace(line.clone()));
    let _ = ctx.reply(line);
}

impl Plugin for Client {
    fn register() -> Result<(), String> {
        counted::serve(Counts).map_err(|e| format!("{e:?}"))?;
        command("greeted")
            .about("Talks to the greeter plugin")
            .then(
                literal("greet").then(argument("name", parser::word()).executes_async(
                    |ctx| async move {
                        let name = ctx.get::<String>("name").unwrap_or_default();
                        greet_through_greeter(ctx, name).await;
                    },
                )),
            )
            .then(literal("answer").executes(|ctx| {
                ctx.reply(ANSWER.with(|answer| answer.borrow().clone()))?;
                Ok(1)
            }))
            .then(literal("last").executes(|ctx| {
                let line = match LAST.with(|last| last.borrow().clone()) {
                    Some((name, total)) => format!("Last greeted {name} ({total})"),
                    None => "Nobody greeted yet".to_owned(),
                };
                ctx.reply(line)?;
                Ok(1)
            }))
            .then(literal("greeter").executes(|ctx| {
                let line = match GREETER_UP.with(|up| *up.borrow()) {
                    Some(true) => "greeter is loaded",
                    Some(false) => "greeter is gone",
                    None => "greeter state unknown",
                };
                ctx.reply(line)?;
                Ok(1)
            }))
            .register()?;
        Ok(())
    }

    fn plugin_change(id: &str, _version: &str, loaded: bool) {
        if id == "greeter" {
            GREETER_UP.with(|up| *up.borrow_mut() = Some(loaded));
        }
    }
}

plugin!(Client {
    id: "messaging-client",
    version: "0.1.0",
    description: "Calls the optional greeter plugin",
    soft_depends: "greeter",
    requests: ["cap.cross-plugin = all"],
});
