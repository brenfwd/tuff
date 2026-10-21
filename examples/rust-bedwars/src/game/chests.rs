// SPDX-License-Identifier: MIT
//! Team chests and ender chests. Both are plugin menus over stashes the
//! match keeps, so the real ender chest of a player is never shown or
//! touched, and everything is dropped when the game ends.

use std::collections::BTreeMap;

use tuff_plugin_sdk::text;
use tuff_plugin_sdk::text::Styled;

use super::{Game, Host, Id, Life, Match, MenuSlot, Phase};
use crate::map::Pos;
use crate::shop::{snbt_count, snbt_id};

/// Item id to how many of it are stored.
pub type Stash = BTreeMap<String, u32>;

const RESOURCES: [&str; 4] = [
    "minecraft:iron_ingot",
    "minecraft:gold_ingot",
    "minecraft:diamond",
    "minecraft:emerald",
];
const ROWS: u8 = 4;
const CAPACITY: usize = 27;
const DEPOSIT_SLOT: u8 = 31;
const REACH: f64 = 6.0;

pub fn is_chest(state: &str) -> bool {
    let id = state.split('[').next().unwrap_or(state);
    matches!(
        id,
        "minecraft:chest" | "minecraft:trapped_chest" | "minecraft:ender_chest"
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Team(usize),
    Ender,
}

impl Match {
    /// Whether the team has players, no bed and nobody left to respawn.
    pub fn eliminated(&self, team: usize) -> bool {
        self.team_size(team) > 0
            && !self.teams[team].bed
            && self
                .members
                .values()
                .filter(|m| m.team == team)
                .all(|m| m.life == Life::Out)
    }

    fn may_open(&self, team: usize, id: Id) -> Result<(), &'static str> {
        if self.team_size(team) == 0 {
            return Err("This chest is locked: nobody lives on this island.");
        }
        if self.eliminated(team) {
            return Ok(());
        }
        match self.members.get(&id) {
            Some(m) if m.team == team && m.life != Life::Out => Ok(()),
            _ => Err("This chest belongs to another team."),
        }
    }
}

fn target(eye: [f64; 3], dir: [f64; 3], spots: &[(Pos, Kind)]) -> Option<Kind> {
    let mut t = 0.0;
    while t <= REACH {
        let at = [
            (eye[0] + dir[0] * t).floor() as i32,
            (eye[1] + dir[1] * t).floor() as i32,
            (eye[2] + dir[2] * t).floor() as i32,
        ];
        if let Some((_, kind)) = spots.iter().find(|(p, _)| *p == at) {
            return Some(*kind);
        }
        t += 0.05;
    }
    None
}

fn slot_item(stash: &Stash, slot: u8) -> Option<(&String, u32)> {
    stash
        .iter()
        .nth(usize::from(slot))
        .filter(|_| usize::from(slot) < CAPACITY)
        .map(|(item, count)| (item, *count))
}

impl Game {
    /// A chest window opened for `id`: when they look at a chest of a
    /// running arena, replaces it with the matching plugin menu.
    pub fn container_opened(&mut self, host: &mut dyn Host, id: Id) {
        let Some((dimension, eye, dir)) = host.gaze(id) else {
            return;
        };
        let Some(i) = self.in_dimension(&dimension) else {
            return;
        };
        let arena = &self.arenas[i];
        let Phase::Running(game) = &arena.phase else {
            return;
        };
        let mut spots = Vec::new();
        for (team, t) in arena.map.teams.iter().enumerate() {
            let [chest, ender] = t.chest_spots();
            spots.push((chest, Kind::Team(team)));
            spots.push((ender, Kind::Ender));
        }
        let Some(kind) = target(eye, dir, &spots) else {
            return;
        };
        let verdict = match kind {
            Kind::Team(team) => game.may_open(team, id),
            Kind::Ender if game.members.get(&id).is_some_and(|m| m.life != Life::Out) => Ok(()),
            Kind::Ender => Err("Only players of the game have an ender chest here."),
        };
        match verdict {
            Ok(()) => self.show_chest(host, id, i, kind),
            Err(line) => {
                host.tell(id, &text!(line.red()));
                let barrier = MenuSlot {
                    slot: 4,
                    item: "minecraft:barrier".into(),
                    count: 1,
                    name: "Locked".into(),
                    lore: vec![line.to_owned()],
                    glint: false,
                };
                host.open_menu(id, "chest:locked", 1, "Locked", &[barrier]);
            }
        }
    }

    fn show_chest(&mut self, host: &mut dyn Host, id: Id, i: usize, kind: Kind) {
        let Phase::Running(game) = &self.arenas[i].phase else {
            return;
        };
        let (menu, title, stash) = match kind {
            Kind::Team(team) => (
                format!("chest:team:{team}"),
                "Team Chest",
                game.chests[team].clone(),
            ),
            Kind::Ender => (
                "chest:ender".to_owned(),
                "Ender Chest",
                game.ender.get(&id).cloned().unwrap_or_default(),
            ),
        };
        let mut items: Vec<MenuSlot> = stash
            .iter()
            .take(CAPACITY)
            .enumerate()
            .map(|(slot, (item, count))| MenuSlot {
                slot: slot as u8,
                item: item.clone(),
                count: (*count).min(64) as u8,
                name: format!("{count} stored"),
                lore: vec!["Click to take a stack.".into()],
                glint: false,
            })
            .collect();
        items.push(MenuSlot {
            slot: DEPOSIT_SLOT,
            item: "minecraft:hopper".into(),
            count: 1,
            name: "Deposit resources".into(),
            lore: vec!["Moves your iron, gold, diamonds and emeralds in.".into()],
            glint: false,
        });
        host.open_menu(id, &menu, ROWS, title, &items);
    }

    pub(super) fn chest_click(&mut self, host: &mut dyn Host, id: Id, menu: &str, slot: u8) {
        let kind = match menu.strip_prefix("chest:") {
            Some("ender") => Kind::Ender,
            Some(rest) => match rest.strip_prefix("team:").and_then(|t| t.parse().ok()) {
                Some(team) => Kind::Team(team),
                None => return,
            },
            None => return,
        };
        let running = |a: &super::Arena| matches!(a.phase, Phase::Running(_));
        let seat = self
            .arenas
            .iter()
            .position(|a| running(a) && a.game().is_some_and(|g| g.members.contains_key(&id)));
        let Some(i) = seat.or_else(|| {
            let Kind::Team(team) = kind else { return None };
            self.arenas
                .iter()
                .position(|a| running(a) && a.game().is_some_and(|g| g.eliminated(team)))
        }) else {
            return;
        };
        let Phase::Running(game) = &mut self.arenas[i].phase else {
            return;
        };
        if let Kind::Team(team) = kind {
            if team >= game.chests.len() || game.may_open(team, id).is_err() {
                return;
            }
        } else if game.members.get(&id).is_none_or(|m| m.life == Life::Out) {
            return;
        }
        let pending = self.deposited.entry(id).or_default();
        let stash = match kind {
            Kind::Team(team) => &mut game.chests[team],
            Kind::Ender => game.ender.entry(id).or_default(),
        };
        if slot == DEPOSIT_SLOT {
            let mut carried: Stash = Stash::new();
            for (_, stack) in host.carried() {
                if let Some(item) = snbt_id(&stack).filter(|i| RESOURCES.contains(i)) {
                    *carried.entry(item.to_owned()).or_default() += snbt_count(&stack);
                }
            }
            let mut moved = 0;
            for (item, have) in carried {
                let free = have.saturating_sub(pending.get(&item).copied().unwrap_or(0));
                if free == 0 {
                    continue;
                }
                host.take(id, &item, free);
                *pending.entry(item.clone()).or_default() += free;
                *stash.entry(item).or_default() += free;
                moved += free;
            }
            if moved > 0 {
                host.tell(id, &text!(format!("Deposited {moved} items.").green()));
            }
        } else if let Some((item, count)) = slot_item(stash, slot).map(|(i, c)| (i.clone(), c)) {
            let amount = count.min(64);
            host.give(id, &item, amount);
            if amount == count {
                stash.remove(&item);
            } else {
                stash.insert(item, count - amount);
            }
        } else {
            return;
        }
        self.show_chest(host, id, i, kind);
    }
}
