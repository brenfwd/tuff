// SPDX-License-Identifier: MIT
//! The bots plugin's typed interfaces (`tuff-bots:api`), generated from
//! `wit/bots.wit`. Consumers call `control::Control` and serve
//! `events::Serve`.

#[rustfmt::skip]
#[allow(dead_code)]
mod api;

pub use api::*;
