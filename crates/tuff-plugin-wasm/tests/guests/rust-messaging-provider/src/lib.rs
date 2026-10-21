//! Test provider for the messaging-client example: serves
//! `tuff-examples:greeter/greet` (see `wit/greeter.wit`) and notifies
//! `counted` after every greeting.

use std::collections::HashMap;

use tuff_plugin_sdk::prelude::*;

#[rustfmt::skip]
#[allow(dead_code)]
pub mod api;

use api::{counted, greet};

#[derive(Default)]
struct Greetings {
    counts: HashMap<String, u32>,
}

impl greet::Serve for Greetings {
    fn greet(&mut self, _from: &str, name: String) -> Result<String, greet::GreetError> {
        if name.trim().is_empty() {
            return Err(greet::GreetError::EmptyName);
        }
        let total = self.counts.entry(name.clone()).or_insert(0);
        *total += 1;
        let total = *total;
        let _ = counted::Counted::any().notify_counted(name.clone(), total);
        Ok(format!("Hello, {name}! (greeting {total})"))
    }
}

struct Greeter;

impl Plugin for Greeter {
    fn register() -> Result<(), String> {
        greet::serve(Greetings::default()).map_err(|e| format!("{e:?}"))
    }
}

plugin!(Greeter {
    id: "greeter",
    version: "1.0.0",
    description: "Typed-interface provider for the messaging tests",
    requests: ["cap.cross-plugin = all"],
});
