// SPDX-License-Identifier: MIT
//! The item shop, team upgrades and what a death does to a player's
//! belongings.

use super::*;
use crate::shop::{self, Category, Gear, Spot};

const UPKEEP: u64 = 2 * SECOND;
const HEAL_RADIUS: f64 = 12.0;
const TRAP_RADIUS: f64 = 8.0;

/// What a player holds when a match begins.
pub(super) fn kit(host: &mut dyn Host, id: Id, color: &str, gear: Gear) {
    for (slot, stack) in shop::leather_set(color, gear) {
        host.set_slot(id, slot, Some(&stack));
    }
    let sword = shop::geared(shop::STARTER_SWORD, 1, gear);
    host.set_slot(id, *shop::HOTBAR.start(), Some(&sword));
}

fn first_free(taken: &BTreeSet<u8>) -> Option<u8> {
    shop::HOTBAR
        .chain(shop::MAIN)
        .find(|slot| !taken.contains(slot))
}

pub(super) fn rank(id: &str) -> u8 {
    ["leather", "chainmail", "iron", "diamond"]
        .iter()
        .position(|m| id.contains(m))
        .map_or(0, |i| i as u8 + 1)
}

impl Game {
    /// The arena and team of a player who is alive in a running match.
    fn seat(&self, id: Id) -> Option<(usize, usize)> {
        self.arenas
            .iter()
            .enumerate()
            .find_map(|(i, a)| match &a.phase {
                Phase::Running(g) => g
                    .members
                    .get(&id)
                    .filter(|m| m.life == Life::Alive)
                    .map(|m| (i, m.team)),
                _ => None,
            })
    }

    fn game_at(&mut self, i: usize) -> &mut Match {
        match &mut self.arenas[i].phase {
            Phase::Running(g) => g,
            _ => unreachable!("seated players are in running matches"),
        }
    }

    fn wallet(
        &mut self,
        host: &mut dyn Host,
        id: Id,
    ) -> (BTreeMap<Resource, u32>, Vec<(u8, String)>) {
        let slots = host.carried();
        let mut wallet = shop::wallet(
            slots
                .iter()
                .filter_map(|(_, s)| Some((shop::snbt_id(s)?, shop::snbt_count(s)))),
        );
        for (resource, spent) in self.spent.get(&id).into_iter().flatten() {
            if let Some(have) = wallet.get_mut(resource) {
                *have = have.saturating_sub(*spent);
            }
        }
        (wallet, slots)
    }

    fn charge(&mut self, host: &mut dyn Host, id: Id, price: shop::Price) {
        host.take(id, price.resource.item(), price.amount);
        *self
            .spent
            .entry(id)
            .or_default()
            .entry(price.resource)
            .or_insert(0) += price.amount;
    }

    /// Shows `id` the shop's `category`.
    pub fn open_shop(&mut self, host: &mut dyn Host, id: Id, category: Category) {
        if self.seat(id).is_none() {
            host.tell(id, &text!("The shop only serves players in a game.".red()));
            return;
        }
        let mut items = Vec::new();
        for c in Category::ALL {
            items.push(MenuSlot {
                slot: shop::tab_slot(c),
                item: shop::category_icon(c).to_owned(),
                count: 1,
                name: c.name().to_owned(),
                lore: Vec::new(),
                glint: c == category,
            });
        }
        items.push(MenuSlot {
            slot: shop::UPGRADES_SLOT,
            item: "minecraft:nether_star".into(),
            count: 1,
            name: "Team upgrades".into(),
            lore: Vec::new(),
            glint: false,
        });
        for (slot, offer) in shop::offer_slots(category) {
            items.push(MenuSlot {
                slot,
                item: shop::item_for(offer, "white"),
                count: offer.count.min(64) as u8,
                name: offer.name.to_owned(),
                lore: vec![format!("Cost: {}", shop::price_text(offer.price))],
                glint: false,
            });
        }
        host.open_menu(
            id,
            &format!("shop:{}", category.name()),
            6,
            "Item Shop",
            &items,
        );
    }

    /// Shows `id` their team's upgrades.
    pub fn open_upgrades(&mut self, host: &mut dyn Host, id: Id) {
        let Some((i, team)) = self.seat(id) else {
            return;
        };
        let state = self.game_at(i).teams[team].clone();
        let mut items = vec![MenuSlot {
            slot: shop::BACK_SLOT,
            item: "minecraft:arrow".into(),
            count: 1,
            name: "Back to the shop".into(),
            lore: Vec::new(),
            glint: false,
        }];
        for (n, up) in shop::UPGRADES.iter().enumerate() {
            let owned = state.tier(up.key);
            let lore = match shop::next_tier(up, owned) {
                Some(price) => format!("Cost: {}", shop::price_text(price)),
                None => "Maxed".to_owned(),
            };
            items.push(MenuSlot {
                slot: shop::upgrade_slot(n),
                item: upgrade_icon(up.key).to_owned(),
                count: 1,
                name: up.name.to_owned(),
                lore: vec![format!("Tier {owned}/{}", up.tiers.len()), lore],
                glint: owned > 0,
            });
        }
        host.open_menu(id, "upgrades", 3, "Team Upgrades", &items);
    }

    /// A click on slot `slot` of the plugin menu `menu`.
    pub fn menu_click(&mut self, host: &mut dyn Host, id: Id, menu: &str, slot: u8) {
        if let Some(name) = menu.strip_prefix("shop:") {
            let Some(current) = Category::of(name) else {
                return;
            };
            if let Some(tab) = shop::tab_at(slot) {
                self.open_shop(host, id, tab);
            } else if slot == shop::UPGRADES_SLOT {
                self.open_upgrades(host, id);
            } else if let Some((_, offer)) = shop::offer_slots(current)
                .into_iter()
                .find(|(s, _)| *s == slot)
            {
                match self.buy(host, id, offer) {
                    Ok(line) => host.tell(id, &line),
                    Err(line) => host.tell(id, &text!(line.as_str().red())),
                }
            }
        } else if menu.starts_with("chest:") {
            self.chest_click(host, id, menu, slot);
        } else if menu == "upgrades" {
            if slot == shop::BACK_SLOT {
                self.open_shop(host, id, Category::Blocks);
            } else if let Some(up) = shop::upgrade_at(slot) {
                match self.buy_upgrade(host, id, up) {
                    Ok(()) => self.open_upgrades(host, id),
                    Err(line) => host.tell(id, &text!(line.as_str().red())),
                }
            }
        }
    }

    pub(super) fn buy(
        &mut self,
        host: &mut dyn Host,
        id: Id,
        offer: &shop::Offer,
    ) -> Result<String, String> {
        let (i, team) = self
            .seat(id)
            .ok_or("The shop only serves players in a game.")?;
        let color = self.arenas[i].map.teams[team].color.clone();
        let gear = self.game_at(i).teams[team].gear();
        let (mut wallet, slots) = self.wallet(host, id);
        shop::pay(&mut wallet, offer.price)?;
        let delivery = shop::delivery(offer, &color, gear);
        let taken: BTreeSet<u8> = slots.iter().map(|(s, _)| *s).collect();
        if offer.category == Category::Armor {
            let worn = slots
                .iter()
                .find(|(s, _)| *s == 8)
                .and_then(|(_, s)| shop::snbt_id(s).map(rank))
                .unwrap_or(0);
            let bought = delivery
                .iter()
                .find_map(|(_, s)| shop::snbt_id(s).map(rank))
                .unwrap_or(0);
            if bought <= worn {
                return Err("You already wear that armor or better.".into());
            }
        }
        let mut free = taken.clone();
        let mut placed = Vec::new();
        for (spot, stack) in &delivery {
            let slot = match spot {
                Spot::Slot(slot) => *slot,
                Spot::Free => first_free(&free).ok_or("Your inventory is full.")?,
            };
            free.insert(slot);
            placed.push((slot, stack.clone()));
        }
        self.charge(host, id, offer.price);
        if placed.is_empty() {
            host.give(id, &shop::item_for(offer, &color), offer.count);
        }
        for (slot, stack) in placed {
            host.set_slot(id, slot, Some(&stack));
        }
        Ok(text!("You purchased ".green(), offer.name.gold()))
    }

    fn buy_upgrade(
        &mut self,
        host: &mut dyn Host,
        id: Id,
        up: &shop::Upgrade,
    ) -> Result<(), String> {
        let (i, team) = self
            .seat(id)
            .ok_or("The shop only serves players in a game.")?;
        let owned = self.game_at(i).teams[team].tier(up.key);
        let price = shop::next_tier(up, owned).ok_or("That upgrade is maxed out.")?;
        let (mut wallet, slots) = self.wallet(host, id);
        shop::pay(&mut wallet, price)?;
        self.charge(host, id, price);
        let state = &mut self.game_at(i).teams[team];
        state.upgrades.insert(up.key.to_owned(), owned + 1);
        if up.key == "forge" {
            state.forge = owned + 1;
        }
        let gear = state.gear();
        if matches!(up.key, "sharpness" | "protection") {
            for (slot, stack) in slots {
                let Some(item) = shop::snbt_id(&stack) else {
                    continue;
                };
                if shop::is_armor(item) || item.ends_with("_sword") {
                    let again = shop::geared(item, shop::snbt_count(&stack), gear);
                    host.set_slot(id, slot, Some(&again));
                }
            }
        }
        let who = match &self.arenas[i].phase {
            Phase::Running(g) => g.members.get(&id).map(|m| m.name.clone()),
            _ => None,
        }
        .unwrap_or_default();
        let line = text!(who.as_str().gray(), " purchased ".yellow(), up.name.gold());
        let mates: Vec<Id> = self
            .game_at(i)
            .members
            .iter()
            .filter(|(_, m)| m.team == team && m.life != Life::Out)
            .map(|(id, _)| *id)
            .collect();
        for mate in mates {
            host.tell(mate, &line);
        }
        Ok(())
    }

    /// `id` died, hurt last by `credit`: the resources they carried go to
    /// that player when an enemy, and they keep their armor and permanent
    /// tools, with a wooden sword.
    pub fn died_by(&mut self, host: &mut dyn Host, id: Id, credit: Option<Id>) {
        self.loot(host, id, credit);
        self.died(host, id);
    }

    fn loot(&mut self, host: &mut dyn Host, id: Id, credit: Option<Id>) {
        let Some((i, team)) = self.seat(id) else {
            return;
        };
        let slots = host.carried();
        let mut totals: BTreeMap<Resource, u32> = BTreeMap::new();
        let mut kept = Vec::new();
        for (slot, stack) in &slots {
            let Some(item) = shop::snbt_id(stack) else {
                continue;
            };
            if let Some(resource) = Resource::of_item(item) {
                *totals.entry(resource).or_insert(0) += shop::snbt_count(stack);
            } else if shop::survives(item) {
                kept.push((*slot, item.to_owned(), shop::snbt_count(stack)));
            }
        }
        let gear = self.game_at(i).teams[team].gear();
        host.clear(id);
        let mut taken: BTreeSet<u8> = BTreeSet::new();
        for (slot, item, count) in kept {
            taken.insert(slot);
            host.set_slot(id, slot, Some(&shop::geared(&item, count, gear)));
        }
        if let Some(slot) = first_free(&taken) {
            let sword = shop::geared(shop::STARTER_SWORD, 1, gear);
            host.set_slot(id, slot, Some(&sword));
        }
        let Some(killer) = credit.filter(|k| *k != id) else {
            return;
        };
        let game = self.game_at(i);
        let enemy = game
            .members
            .get(&killer)
            .is_some_and(|m| m.team != team && m.life == Life::Alive);
        if !enemy {
            return;
        }
        for (resource, count) in totals {
            host.give(killer, resource.item(), count);
            let line = text!(format!("+{count} {}", resource.name()).color(resource.color()));
            host.tell(killer, &line);
        }
    }

    /// Effects of team upgrades, refreshed every couple of seconds.
    pub(super) fn upkeep(&mut self, host: &mut dyn Host, i: usize) {
        if !self.now.is_multiple_of(UPKEEP) {
            return;
        }
        let arena = &mut self.arenas[i];
        let Phase::Running(game) = &mut arena.phase else {
            return;
        };
        let alive: Vec<(Id, usize)> = game
            .members
            .iter()
            .filter(|(_, m)| m.life == Life::Alive)
            .map(|(id, m)| (*id, m.team))
            .collect();
        let near = |host: &mut dyn Host, id: Id, at: [f64; 3], radius: f64| {
            host.position(id).is_some_and(|p| {
                let (dx, dz) = (p[0] - at[0], p[2] - at[2]);
                dx * dx + dz * dz <= radius * radius && (p[1] - at[1]).abs() <= radius
            })
        };
        for &(id, team) in &alive {
            let state = &game.teams[team];
            let haste = state.tier("haste");
            if haste > 0 {
                host.effect(id, "minecraft:haste", UPKEEP as i32 + 40, haste as u8 - 1);
            }
            let home = center(arena.map.teams[team].spawn);
            if state.tier("heal_pool") > 0 && near(host, id, home, HEAL_RADIUS) {
                host.effect(id, "minecraft:regeneration", UPKEEP as i32 + 40, 0);
            }
        }
        for team in 0..game.teams.len() {
            if game.teams[team].tier("trap") == 0 {
                continue;
            }
            let home = center(arena.map.teams[team].spawn);
            let intruder = alive
                .iter()
                .find(|(id, t)| *t != team && near(host, *id, home, TRAP_RADIUS))
                .map(|(id, _)| *id);
            let Some(intruder) = intruder else {
                continue;
            };
            game.teams[team].upgrades.remove("trap");
            host.effect(intruder, "minecraft:blindness", 160, 0);
            host.effect(intruder, "minecraft:slowness", 160, 1);
            let line = text!(
                "TRAP > ".red().bold(),
                "Someone walked into your trap!".yellow()
            );
            for &(id, t) in &alive {
                if t == team {
                    host.tell(id, &line);
                }
            }
        }
    }
}

fn upgrade_icon(key: &str) -> &'static str {
    match key {
        "sharpness" => "minecraft:iron_sword",
        "protection" => "minecraft:iron_chestplate",
        "haste" => "minecraft:iron_pickaxe",
        "forge" => "minecraft:furnace",
        "heal_pool" => "minecraft:beacon",
        _ => "minecraft:tripwire_hook",
    }
}
