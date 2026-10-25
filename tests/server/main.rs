//! Integration tests that share one process: each module drives the server
//! or a workspace crate. Tests needing process isolation keep their own binary.

extern crate tuff_dylib;

#[path = "../common/mod.rs"]
mod common;

#[path = "../../benches/common/console.rs"]
mod console;
#[expect(dead_code, reason = "helpers used by only some targets")]
#[path = "../../benches/stress/options.rs"]
mod options;
#[path = "../../benches/common/servers.rs"]
mod servers;
#[expect(dead_code, reason = "helpers used by only some targets")]
#[path = "../../benches/common/vanilla.rs"]
mod vanilla;
#[path = "../../benches/common/watchers.rs"]
mod watchers;

mod access_control;
mod admin_access;
mod admin_backups;
mod admin_config;
mod admin_packs;
mod admin_panel;
mod admin_players;
mod admin_world_map;
mod bench_vanilla;
mod bench_watchers;
mod block_click_items;
mod block_doors;
mod block_from_items;
mod bossbar;
mod bot;
mod bucket_conservation;
mod console_commands;
mod custom_pack;
mod data_commands;
mod datapack_dimensions;
mod datapack_registries;
mod debugconfig_command;
mod dimension_settings;
mod dylib_linking;
mod entity_from_items;
mod fetch_profile;
mod file_size;
mod fill_biome;
mod fill_limit;
mod forceload_ticks;
mod gameplay_properties;
mod gamerule_time;
mod gametest_server;
mod http_wiring;
mod inert_core;
mod item_conservation;
mod minecart_improvements;
mod network_ids;
mod no_println;
mod no_save;
mod no_serve;
mod outbound_bytes;
mod packet_replay;
mod packet_tracing;
mod parity_matrix;
mod parity_run;
mod place_command;
mod pre_login_limits;
mod property_overrides;
mod resource_packs;
mod restart_command;
mod save_ordering;
mod scoreboard;
mod server_list_and_login;
mod server_properties;
mod serverpack_command;
mod sql_console;
mod sqlite_world;
mod test_layout;
mod tick_samples;
mod time_weather_rules;
mod timings;
mod tuff_config;
mod vanilla_drops;
mod velocity_forwarding;
mod watchdog_coverage;
mod water_bucket;
mod waypoints;
mod world_lifecycle;
