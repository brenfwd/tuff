//! Registers every vanilla command this crate implements.

use tuff_core::command::ASK_SERVER;
use tuff_core::world::commands::Registry;

use super::{
    admin, advancement, attribute, bossbar, camera, compute, damage, data, datapack, execute,
    function, gamemode, gametest, item, jfr, locate, loot, orient, particle, place, players,
    recipe, ride, schedule, scoreboard, sequences, spreadplayers, tick, time, waypoint, world,
    worldborder,
};

const SUMMONABLE: &str = "minecraft:summonable_entities";
const POST_EFFECTS: &str = "minecraft:post_effects";
const SOUNDS: &str = "minecraft:available_sounds";

/// Adds the vanilla commands to `r`. `core-commands` registers the
/// administration set (stop, save-all, op, deop, whitelist and the ban
/// lists).
struct BookText;

impl tuff_core::world::commands::TextResolver for BookText {
    fn resolve<'a>(
        &'a self,
        ctx: &'a mut tuff_core::world::commands::Context<'_>,
        component: &'a tuff_item::text::TextComponent,
    ) -> tuff_core::world::commands::TextFuture<'a> {
        Box::pin(crate::resolve::resolve(ctx, component))
    }
}

pub fn register(r: &mut Registry) {
    r.set_text_resolver(std::sync::Arc::new(BookText));
    r.vanilla("gamemode", &[], gamemode::Gamemode);
    r.vanilla("defaultgamemode", &[], gamemode::DefaultGamemode);
    r.vanilla(
        "time",
        &[("timemarker", ASK_SERVER), ("timeline", ASK_SERVER)],
        time::Time,
    );
    r.vanilla("give", &[], players::Give);
    r.vanilla("fetchprofile", &[], players::FetchProfile);
    r.vanilla("clear", &[], players::Clear);
    r.vanilla("kill", &[], players::Kill);
    r.vanilla("teleport", &[], players::TeleportCommand);
    r.register_alias("tp");
    r.vanilla("experience", &[], players::ExperienceCommand);
    r.register_alias("xp");
    r.vanilla("effect", &[], players::Effect);
    r.vanilla("enchant", &[], players::Enchant);
    r.vanilla("spawnpoint", &[], players::SpawnPoint);
    r.vanilla("setworldspawn", &[], players::SetWorldSpawn);
    r.vanilla("setidletimeout", &[], admin::SetIdleTimeout);
    r.vanilla("title", &[], players::Title);
    r.vanilla("tellraw", &[], players::Tellraw);
    r.vanilla("playsound", &[("sound", SOUNDS)], players::PlaySound);
    r.vanilla("stopsound", &[("sound", SOUNDS)], players::StopSound);
    r.vanilla("kick", &[], players::Kick);
    r.vanilla("transfer", &[], players::Transfer);
    r.vanilla("msg", &[], players::Msg);
    r.register_alias("tell");
    r.register_alias("w");
    r.vanilla("list", &[], players::List);
    r.vanilla("setblock", &[], world::SetBlock);
    r.vanilla("fill", &[], world::Fill);
    r.vanilla("fillbiome", &[], world::FillBiome);
    r.vanilla("clone", &[], world::Clone);
    r.vanilla("summon", &[("entity", SUMMONABLE)], world::Summon);
    r.vanilla("ride", &[], ride::Ride);
    r.vanilla("attribute", &[], attribute::AttributeCmd);
    r.vanilla("forceload", &[], world::ForceLoad);
    r.vanilla("locate", &[], locate::Locate);
    r.vanilla("execute", &[], execute::Execute);
    r.vanilla("gamerule", &[], admin::GameRule);
    r.vanilla("compute", &[], compute::Compute);
    r.vanilla("weather", &[], admin::Weather);
    r.vanilla("worldborder", &[], worldborder::WorldBorder);
    r.vanilla("difficulty", &[], admin::DifficultyCommand);
    r.vanilla("seed", &[], admin::Seed);
    r.vanilla("version", &[], admin::Version);
    r.vanilla("tick", &[], tick::Tick);
    r.vanilla("rotate", &[], orient::Rotate);
    r.vanilla("swing", &[], orient::Swing);
    r.vanilla(
        "posteffect",
        &[("posteffect", POST_EFFECTS)],
        camera::PostEffect,
    );
    r.vanilla("spectate", &[], camera::Spectate);
    r.vanilla("dialog", &[], camera::Dialog);
    r.vanilla("random", &[("sequence", ASK_SERVER)], sequences::Random);
    r.vanilla("stopwatch", &[("id", ASK_SERVER)], sequences::Stopwatch);
    r.vanilla(
        "say",
        &[],
        admin::Chat {
            chat_type: "minecraft:say_command",
            argument: "message",
        },
    );
    r.vanilla(
        "me",
        &[],
        admin::Chat {
            chat_type: "minecraft:emote_command",
            argument: "action",
        },
    );
    r.vanilla("advancement", &[], advancement::AdvancementCommand);
    r.vanilla("recipe", &[], recipe::RecipeCommand);
    r.vanilla("help", &[], admin::Help);
    r.vanilla(
        "scoreboard",
        &[
            ("targets", ASK_SERVER),
            ("target", ASK_SERVER),
            ("source", ASK_SERVER),
        ],
        scoreboard::ScoreboardCommand,
    );
    r.vanilla("team", &[("members", ASK_SERVER)], scoreboard::TeamCommand);
    r.vanilla("trigger", &[], scoreboard::TriggerCommand);
    r.vanilla("data", &[("target", ASK_SERVER)], data::Data);
    r.vanilla("tag", &[], data::TagCommand);
    r.vanilla("teammsg", &[], data::TeamMessage);
    r.register_alias("tm");
    r.vanilla(
        "function",
        &[("name", ASK_SERVER)],
        function::FunctionCommand,
    );
    r.vanilla("return", &[], function::ReturnCommand);
    r.vanilla(
        "schedule",
        &[("function", ASK_SERVER)],
        schedule::ScheduleCommand,
    );
    r.vanilla(
        "datapack",
        &[("name", ASK_SERVER), ("existing", ASK_SERVER)],
        datapack::DataPackCommand,
    );
    r.vanilla("reload", &[], datapack::Reload);
    r.vanilla("place", &[("template", ASK_SERVER)], place::Place);
    r.vanilla("test", &[("tests", ASK_SERVER)], gametest::TestCommand);
    r.vanilla("particle", &[], particle::Particle);
    r.vanilla("item", &[], item::ItemCommand);
    r.vanilla("damage", &[], damage::Damage);
    r.vanilla("loot", &[], loot::Loot);
    r.vanilla("debug", &[("name", ASK_SERVER)], function::DebugCommand);
    r.register(jfr::root(), jfr::Jfr);
    r.vanilla("spreadplayers", &[], spreadplayers::SpreadPlayers);
    r.vanilla(
        "waypoint",
        &[("waypoint", ASK_SERVER)],
        waypoint::WaypointCommand,
    );
    r.vanilla("bossbar", &[("id", ASK_SERVER)], bossbar::BossBarCommand);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::world::commands::Registry;

    #[tuff_core::watchdog]
    #[test]
    fn every_registered_command_and_alias_has_an_executor() {
        let mut registry = Registry::new();
        register(&mut registry);
        tuff_feature_builtin_core_commands::register(&mut registry);
        for command in registry.roots().commands() {
            assert!(
                registry.executor(command.name).is_some(),
                "/{} parses but nothing runs it",
                command.name
            );
        }
        for alias in ["tp", "xp", "tell", "w"] {
            assert!(registry.roots().command(alias).is_some(), "{alias}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn posteffect_advertises_the_client_post_effects_provider() {
        let mut registry = Registry::new();
        register(&mut registry);
        let command = registry.roots().command("posteffect").expect("registered");
        assert!(
            command
                .providers
                .contains(&("posteffect", "minecraft:post_effects"))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn tick_requires_admin_permission() {
        use tuff_core::command::allowed;
        use tuff_storage::admin::PermissionLevel;

        let mut registry = Registry::new();
        register(&mut registry);
        let tick = registry.roots().command("tick").expect("tick registered");
        assert!(!allowed(tick.node, &PermissionLevel::Gamemasters));
        assert!(allowed(tick.node, &PermissionLevel::Admins));
    }

    #[tuff_core::watchdog]
    #[test]
    fn version_requires_gamemaster_permission() {
        use tuff_core::command::allowed;
        use tuff_storage::admin::PermissionLevel;

        let mut registry = Registry::new();
        register(&mut registry);
        let version = registry.roots().command("version").expect("registered");
        assert!(!allowed(version.node, &PermissionLevel::Moderators));
        assert!(allowed(version.node, &PermissionLevel::Gamemasters));
    }
}
