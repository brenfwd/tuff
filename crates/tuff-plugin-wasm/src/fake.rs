//! Fake players a plugin owns: admitting, driving and reading them. The
//! sandbox keeps one handle per fake; state reads come from the watch
//! each fake's driver publishes every tick, never from the world.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::oneshot;
use tuff_core::world::Shared;
use tuff_core::world::fake::actions::FakeAction;
use tuff_core::world::fake::motion::Held;
use tuff_core::world::fake::{self, FakeHandle, FakeSpec, Model};
use tuff_core::world::messages::CoordinatorMessage;
use tuff_world_model::{BlockPos, Face};

use crate::sandbox::{uuid, uuid_of};
use crate::wit::bindings::types::{
    BreakTool, Direction, FakeAction as WitAction, FakeError, FakeInput, FakeSpec as WitSpec,
    FakeState, Inventory, ItemStack as WitStack, Rotation, Slot, Uuid, Vec3,
};
use crate::world::Env;

const HOTBAR_START: u8 = 36;

/// The fakes one sandbox owns.
#[derive(Default)]
pub struct Fakes {
    owned: HashMap<u128, FakeHandle>,
}

impl Fakes {
    fn live(&mut self) {
        self.owned
            .retain(|_, fake| fake.observed.has_changed().is_ok());
    }

    pub fn uuids(&mut self) -> Vec<Uuid> {
        self.live();
        let mut all: Vec<u128> = self.owned.keys().copied().collect();
        all.sort_unstable();
        all.into_iter().map(uuid).collect()
    }

    pub fn spawn(&mut self, env: &Env, owner: &str, spec: WitSpec) -> Result<Uuid, FakeError> {
        self.live();
        if self.owned.len() >= fake::OWNER_LIMIT {
            return Err(FakeError::LimitReached);
        }
        if spec.name.is_empty()
            || spec.name.len() > 16
            || !spec
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(FakeError::Invalid("name".into()));
        }
        let position = match spec.pos {
            Some(Vec3 { x, y, z }) if [x, y, z].iter().all(|v| v.is_finite()) => Some([x, y, z]),
            Some(_) => return Err(FakeError::Invalid("pos".into())),
            None => None,
        };
        let dimension = match &spec.dimension {
            Some(key) => {
                let id = env.dimension(key).ok_or(FakeError::UnknownDimension)?;
                Some(env.shared.dimensions.get(id).key.clone())
            }
            None => None,
        };
        let core = FakeSpec {
            name: spec.name,
            uuid: spec.uuid.map(uuid_of),
            model: if spec.slim_skin {
                Model::Slim
            } else {
                Model::Wide
            },
            dimension,
            position,
            rotation: spec.rotation.map(|r| (r.yaw, r.pitch)),
            game_mode: spec.game_mode.map(crate::player_state::game_mode_of),
            listed: spec.listed,
            ..FakeSpec::new("")
        };
        let shared = env.shared.clone();
        let owner = owner.to_owned();
        let made = env
            .wait(async move {
                let (reply, handle) = oneshot::channel();
                shared
                    .coordinator
                    .send(CoordinatorMessage::SpawnFake {
                        owner,
                        spec: core,
                        reply,
                    })
                    .await
                    .ok()?;
                handle.await.ok()
            })
            .flatten()
            .ok_or(FakeError::Unavailable)?;
        match made {
            Ok(handle) => {
                let id = handle.uuid;
                self.owned.insert(id, handle);
                Ok(uuid(id))
            }
            Err(reason) if reason.contains("fake players") => Err(FakeError::LimitReached),
            Err(reason) if reason.contains("already online") => Err(FakeError::NameTaken),
            Err(reason) => Err(FakeError::Invalid(reason)),
        }
    }

    pub fn remove(&mut self, env: &Env, owner: &str, fake: Uuid) -> Result<(), FakeError> {
        let id = uuid_of(fake);
        if self.owned.remove(&id).is_none() {
            return Err(FakeError::NotOwner);
        }
        let shared = env.shared.clone();
        let owner = owner.to_owned();
        env.wait(async move {
            let _ = remove(&shared, &owner, Some(id)).await;
        });
        Ok(())
    }

    pub fn input(&mut self, fake: Uuid, input: FakeInput) -> Result<(), FakeError> {
        let handle = self.get(fake)?;
        handle.hold(Held {
            forward: input.forward.clamp(-1, 1),
            strafe: input.strafe.clamp(-1, 1),
            jump: input.jump,
            sprint: input.sprint,
            sneak: input.sneak,
            yaw: input.yaw,
            pitch: input.pitch,
        });
        Ok(())
    }

    pub fn act(&mut self, env: &Env, fake: Uuid, action: WitAction) -> Result<(), FakeError> {
        let action = match action {
            WitAction::Look(Rotation { yaw, pitch }) => FakeAction::Look { yaw, pitch },
            WitAction::Swing => FakeAction::Swing,
            WitAction::Attack(target) => FakeAction::Attack(entity_id(env, target)?),
            WitAction::Interact(interact) => FakeAction::Interact {
                entity: entity_id(env, interact.entity)?,
                off_hand: interact.off_hand,
            },
            WitAction::Dig(dig) => FakeAction::Dig {
                pos: block(dig.pos),
                face: face(dig.face),
            },
            WitAction::AbortDig => FakeAction::AbortDig,
            WitAction::UseItemOn(on) => FakeAction::UseItemOn {
                off_hand: on.off_hand,
                pos: block(on.pos),
                face: face(on.face),
                cursor: [on.cursor.x as f32, on.cursor.y as f32, on.cursor.z as f32],
            },
            WitAction::UseItem(off_hand) => FakeAction::UseItem { off_hand },
            WitAction::ReleaseUse => FakeAction::ReleaseUse,
            WitAction::SelectSlot(slot) if slot < 9 => FakeAction::SelectSlot(i16::from(slot)),
            WitAction::SelectSlot(_) => return Err(FakeError::Invalid("slot".into())),
            WitAction::SwapHands => FakeAction::SwapHands,
            WitAction::Drop(stack) => FakeAction::Drop { stack },
            WitAction::Respawn => FakeAction::Respawn,
        };
        self.get(fake)?.act(action);
        Ok(())
    }

    pub fn state(&mut self, env: &Env, fake: Uuid) -> Option<FakeState> {
        self.live();
        let handle = self.owned.get(&uuid_of(fake))?;
        let observed = handle.observed.borrow();
        let body = &observed.body;
        let dimension = body.dimension?;
        let (position, rotation) = if body.position == [0.0; 3] {
            (observed.position, observed.rotation)
        } else {
            (body.position, body.rotation)
        };
        let [x, y, z] = position;
        let [vx, vy, vz] = body.velocity;
        Some(FakeState {
            dimension: env.shared.dimensions.get(dimension).key.clone(),
            pos: Vec3 { x, y, z },
            velocity: Vec3 {
                x: vx,
                y: vy,
                z: vz,
            },
            yaw: rotation.0,
            pitch: rotation.1,
            on_ground: body.on_ground,
            in_fluid: body.in_fluid,
            horizontal_collision: body.collided,
            health: body.health,
            food: body.food,
            selected_slot: body.selected_slot,
            hotbar: body
                .hotbar
                .iter()
                .map(|item| item.map(|item| item.name().to_owned()))
                .collect(),
            dig_progress: body.dig_progress,
            alive: body.alive,
        })
    }

    pub fn inventory(&mut self, fake: Uuid) -> Option<Inventory> {
        self.live();
        let handle = self.owned.get(&uuid_of(fake))?;
        let observed = handle.observed.borrow();
        let body = &observed.body;
        let stack = |stack: &tuff_item::item::ItemStack| WitStack {
            item: stack.item.name().to_owned(),
            count: stack.count,
        };
        let held = HOTBAR_START + body.selected_slot;
        Some(Inventory {
            held: body
                .items
                .iter()
                .find(|(slot, _)| *slot == held)
                .map(|(_, s)| stack(s)),
            slots: body
                .items
                .iter()
                .map(|(index, s)| Slot {
                    index: *index,
                    stack: stack(s),
                })
                .collect(),
        })
    }

    /// The hotbar slot that breaks `state` fastest and how many ticks it
    /// takes standing, from the fake's last published gear and effects.
    pub fn break_ticks(&mut self, fake: Uuid, state: &str) -> Option<BreakTool> {
        self.live();
        let handle = self.owned.get(&uuid_of(fake))?;
        let parsed = crate::registry::state(state)?;
        let hardness = parsed.destroy_speed()?;
        let observed = handle.observed.borrow();
        let body = &observed.body;
        let miner = tuff_core::world::player::Miner {
            on_ground: true,
            ..body
                .miner
                .unwrap_or_else(|| tuff_core::world::player::Miner::plain(true))
        };
        best_tool(parsed, hardness, &body.items, &miner, body.selected_slot)
    }

    fn get(&mut self, fake: Uuid) -> Result<&FakeHandle, FakeError> {
        self.live();
        self.owned.get(&uuid_of(fake)).ok_or(FakeError::NotOwner)
    }
}

/// The hotbar slot whose stack breaks `state` in the fewest ticks, the
/// selected one on a tie, then the lowest.
fn best_tool(
    state: tuff_vanilla_data::BlockState,
    hardness: f32,
    items: &[(u8, tuff_item::item::ItemSnapshot)],
    miner: &tuff_core::world::player::Miner,
    selected: u8,
) -> Option<BreakTool> {
    let mut best: Option<BreakTool> = None;
    for slot in 0..9u8 {
        let tool = items
            .iter()
            .find(|(index, _)| *index == HOTBAR_START + slot)
            .map(|(_, stack)| &**stack);
        let progress = tuff_core::world::player::destroy_progress_at(
            state,
            Some(hardness),
            tool,
            miner,
            false,
        );
        if progress <= 0.0 {
            continue;
        }
        let ticks = (1.0 / progress).ceil().max(1.0) as u32;
        let better = best.is_none_or(|b| ticks < b.ticks || (ticks == b.ticks && slot == selected));
        if better {
            best = Some(BreakTool { ticks, slot });
        }
    }
    best
}

/// Asks every fake of `owner` (or one) to leave; returns how many were asked.
pub async fn remove(shared: &Arc<Shared>, owner: &str, uuid: Option<u128>) -> usize {
    let (reply, count) = oneshot::channel();
    let sent = shared
        .coordinator
        .send(CoordinatorMessage::RemoveFake {
            owner: owner.to_owned(),
            uuid,
            reply,
        })
        .await;
    if sent.is_err() {
        return 0;
    }
    count.await.unwrap_or(0)
}

fn entity_id(env: &Env, target: Uuid) -> Result<i32, FakeError> {
    let wanted = uuid_of(target);
    let shared = env.shared.clone();
    env.wait(async move {
        let players = crate::player_state::snapshots(&shared).await;
        if let Some(player) = players.iter().find(|p| p.uuid == wanted) {
            return Some(player.id);
        }
        crate::entities::on_entity(&shared, wanted, |_, id| id).await
    })
    .ok_or(FakeError::Unavailable)?
    .ok_or(FakeError::UnknownEntity)
}

fn block(pos: crate::wit::bindings::types::BlockPos) -> BlockPos {
    BlockPos {
        x: pos.x,
        y: pos.y,
        z: pos.z,
    }
}

fn face(direction: Direction) -> Face {
    match direction {
        Direction::Down => Face::Down,
        Direction::Up => Face::Up,
        Direction::North => Face::North,
        Direction::South => Face::South,
        Direction::West => Face::West,
        Direction::East => Face::East,
    }
}

#[cfg(test)]
mod tests {
    use tuff_core::world::player::Miner;
    use tuff_item::item::{ItemSnapshot, ItemStack};
    use tuff_vanilla_data::{Block, Item};

    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_fastest_hotbar_tool_wins_and_the_hand_counts_too() {
        let stone = Block::Stone.default_state();
        let hardness = stone.destroy_speed().unwrap();
        let miner = Miner::plain(true);
        let items = vec![
            (36, ItemSnapshot::of(&ItemStack::new(Item::Cobblestone, 10))),
            (39, ItemSnapshot::of(&ItemStack::new(Item::IronPickaxe, 1))),
            (
                40,
                ItemSnapshot::of(&ItemStack::new(Item::WoodenPickaxe, 1)),
            ),
        ];
        let best = best_tool(stone, hardness, &items, &miner, 0).unwrap();
        assert_eq!(best.slot, 3);
        assert_eq!(best.ticks, 8);
        let bare = best_tool(stone, hardness, &[], &miner, 4).unwrap();
        assert_eq!(
            bare,
            BreakTool {
                ticks: 150,
                slot: 4
            }
        );
        let wool = Block::WhiteWool.default_state();
        let soft = best_tool(wool, wool.destroy_speed().unwrap(), &items, &miner, 0).unwrap();
        assert_eq!(soft, BreakTool { ticks: 24, slot: 0 });
    }
}
