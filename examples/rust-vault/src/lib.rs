// SPDX-License-Identifier: MIT
//! The vault plugin: each player gets a vault that keeps real items for
//! them while the plugin is loaded. Build it with `cargo build --release`
//! and load `target/wasm32-unknown-unknown/release/*.wasm` with
//! `/tuff plugin load`.
//!
//! - `/vault deposit <item> <count>` moves that many items from your
//!   inventory into your vault
//! - `/vault withdraw` gives you everything in your vault
//! - `/vault` lists what it holds
//!
//! Vaults are stashes ([`Stash`]): the server keeps the items and the
//! plugin only moves them, so a vault can never copy or lose one. What a
//! vault still holds when the plugin unloads is used up.

use std::cell::RefCell;
use std::collections::HashMap;

use tuff_plugin_sdk::items::Stash;
use tuff_plugin_sdk::prelude::*;

thread_local! {
    static VAULTS: RefCell<HashMap<(u64, u64), Stash>> = RefCell::new(HashMap::new());
}

fn key(player: &Player) -> (u64, u64) {
    (player.uuid.high, player.uuid.low)
}

/// The player's vault, made on first use.
fn with_vault<T>(player: &Player, f: impl FnOnce(&Stash) -> T) -> Result<T, String> {
    VAULTS.with(|vaults| {
        let mut vaults = vaults.borrow_mut();
        if !vaults.contains_key(&key(player)) {
            let vault = Stash::new().map_err(|err| format!("no vault: {err:?}"))?;
            vaults.insert(key(player), vault);
        }
        Ok(f(&vaults[&key(player)]))
    })
}

struct Vault;

impl Plugin for Vault {
    fn register() -> Result<(), String> {
        command("vault")
            .about("Keeps items for you")
            .executes(|ctx| {
                let player = ctx.player()?;
                let contents = with_vault(player, Stash::contents)?;
                if contents.is_empty() {
                    ctx.reply("Your vault is empty")?;
                }
                for stack in &contents {
                    ctx.reply(format!("{} x{}", stack.item, stack.count))?;
                }
                Ok(contents.len() as i32)
            })
            .then(
                literal("deposit").then(
                    argument("item", parser::item()).then(
                        argument("count", parser::integer_in(1..=2304)).executes(|ctx| {
                            let player = ctx.player()?;
                            let item: String = ctx.get("item")?;
                            let item = item.split('[').next().unwrap_or(&item).to_owned();
                            let count: i32 = ctx.get("count")?;
                            let uuid = player.uuid;
                            with_vault(player, |vault| {
                                vault.take_from(uuid, &item, count as u32, true)
                            })?
                            .map_err(|err| format!("cannot deposit: {err:?}"))?;
                            ctx.reply(format!("Depositing {count} {item}"))?;
                            Ok(count)
                        }),
                    ),
                ),
            )
            .then(literal("withdraw").executes(|ctx| {
                let player = ctx.player()?;
                let vault = VAULTS.with(|vaults| vaults.borrow_mut().remove(&key(player)));
                let Some(vault) = vault else {
                    ctx.reply("Your vault is empty")?;
                    return Ok(0);
                };
                let count = vault.count();
                vault
                    .give(player.uuid)
                    .map_err(|err| format!("cannot withdraw: {err:?}"))?;
                ctx.reply(format!("Withdrew {count} items"))?;
                Ok(count as i32)
            }))
            .help()
            .register()?;
        Ok(())
    }
}

plugin!(Vault {
    id: "vault",
    version: "0.1.0",
    description: "Keeps items for players without ever copying one",
});
