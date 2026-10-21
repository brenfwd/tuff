// SPDX-License-Identifier: MIT
//! The __ID__ plugin. Build it with `cargo build --release` and load
//! `target/wasm32-unknown-unknown/release/*.wasm` with `/tuff plugin load`.
//!
//! - `/__ID__` greets you and counts your visits (kept across restarts)
//! - `/__ID__ later` greets you again two seconds later
//! - `/__ID__ reset` forgets your visits (operators)
//! - `/__ID__ help` lists these
//! - joining players get a welcome, diamond finds are announced with a
//!   sparkle of dust particles, and a tip
//!   is broadcast every five minutes
//! - owners change the greeting with the `plugins.__ID__.greeting` setting

use tuff_plugin_sdk::prelude::*;

static GREETING: Setting<String> = Setting::new("greeting", "Hello", "What /__ID__ says");
static VISITS: StoredMap<u32> = StoredMap::new("visits");

struct __NAME__;

impl Plugin for __NAME__ {
    fn register() -> Result<(), String> {
        GREETING.declare()?;
        GREETING.on_change(|greeting| {
            broadcast(&text!("The greeting is now ", greeting.gold()));
        })?;

        command("__ID__")
            .about("Greets you")
            .executes(|ctx| {
                let player = ctx.player()?;
                let visits = VISITS.update(player.uuid, |n| n.unwrap_or(0) + 1)?;
                ctx.reply(text!(
                    GREETING.get().gold(),
                    ", ",
                    player.name.aqua(),
                    "! ",
                    format!("(visit {visits})").gray()
                ))?;
                Ok(visits as i32)
            })
            .then(
                literal("later")
                    .about("Greets you again in two seconds")
                    .executes_async(|ctx| async move {
                        let _ = ctx.reply("See you in two seconds");
                        sleep(40).await;
                        let name = ctx.sender.as_ref().map_or("console", |p| &p.name);
                        let line = text!(GREETING.get().gold(), " again, ", name.aqua(), "!");
                        let _ = ctx.reply(line);
                    }),
            )
            .then(
                literal("reset")
                    .about("Forgets your visits")
                    .requires("__ID__.reset", 2)
                    .executes(|ctx| {
                        VISITS.remove(ctx.player()?.uuid)?;
                        ctx.reply("Your visits are forgotten")?;
                        Ok(1)
                    }),
            )
            .help()
            .register()?;

        on::<ev::Join>(|player| {
            let _ = player.tell(text!("Welcome! Say hi with ", "/__ID__".gold()));
        })?;
        listen::<ev::BlockBreak>()
            .in_dimension(world::OVERWORLD)
            .block("minecraft:diamond_ore")
            .by_players()
            .run(|change| {
                if let Some(player) = &change.player {
                    let diamonds = "diamonds".aqua().bold();
                    broadcast(&text!(player.name.aqua(), " found ", diamonds, "!"));
                    let at = vec3(
                        f64::from(change.pos.x) + 0.5,
                        f64::from(change.pos.y) + 0.5,
                        f64::from(change.pos.z) + 0.5,
                    );
                    let sparkle = Particle::dust(0x55ffff, 1.5, at).spread(vec3(0.3, 0.3, 0.3), 0.0, 20);
                    let _ = World::new(change.dimension.clone()).particles(&sparkle);
                }
            })?;
        every(20 * 60 * 5, || {
            broadcast(&text!("Tip: ".green(), "say hi with /__ID__"));
        });
        Ok(())
    }
}

plugin!(__NAME__ {
    id: "__ID__",
    version: "0.1.0",
    description: "Greets players and counts their visits",
});
