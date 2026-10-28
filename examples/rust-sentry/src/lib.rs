// SPDX-License-Identifier: MIT
//! The sentry plugin: a custom entity shown as an iron golem with an
//! interaction hitbox part.
//!
//! - `/sentry` spawns the custom `sentry:guard` entity at your feet
//! - hitting it (or its hitbox part) tells the player; needs `cap.content`
//!   for the `sentry` namespace and `cap.world-write`

use tuff_plugin_sdk::prelude::*;

struct Sentry;

impl Plugin for Sentry {
    fn content(content: ContentBuilder) -> ContentBuilder {
        content.entity_with_parts(
            "sentry:guard",
            "minecraft:iron_golem",
            &[("minecraft:interaction", "{width:1.4f,height:2.7f}")],
        )
    }

    fn register() -> Result<(), String> {
        command("sentry")
            .about("Spawns a sentry at your feet")
            .executes(|ctx| {
                ctx.player()?;
                let (Some(at), Some(world)) = (acting::location(), acting::world()) else {
                    return Ok(0);
                };
                let yaw = at.rotation.map_or(0.0, |r| r.yaw);
                let _ = world.spawn_custom("sentry:guard", at.pos, yaw);
                Ok(1)
            })
            .register()?;
        on::<ev::PlayerAttack>(|attack| {
            if attack.custom.as_deref() == Some("sentry:guard") {
                let _ = attack.player.tell("The sentry stands firm");
            }
        })?;
        Ok(())
    }
}

plugin!(Sentry {
    id: "sentry",
    version: "0.1.0",
    description: "A custom sentry entity",
});
