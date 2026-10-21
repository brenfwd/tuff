#![recursion_limit = "512"]
extern crate self as tuff_core;

pub mod admin;
pub mod auth;
pub mod command;
pub mod config;
pub mod custom;
pub mod custom_pack;
pub mod datapack;
pub mod dialog;
pub mod exit_code;
pub mod http;
pub use log;
pub mod code_of_conduct;
pub mod crash_report;
pub mod feature;
pub mod logging;
pub mod metrics;
pub mod minimessage;
pub mod mth;
pub mod net;
pub mod panel_host;
pub mod permission;
pub mod profile_text;
pub mod property_spec;
pub mod protocol;
pub mod public_ip;
pub mod query;
pub mod registry;
pub mod resource_pack;
pub mod restart;
pub mod server;
pub mod status;
pub mod storage;
pub mod text_filter;
pub mod threads;
pub mod trace;
pub mod trace_decode;
pub mod watchdog;
pub use tuff_macros::watchdog;
#[cfg(test)]
mod light_fixture_tests;
pub mod world;

pub use server::{Server, ServerHandle};
