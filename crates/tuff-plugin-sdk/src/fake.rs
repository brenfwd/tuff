// SPDX-License-Identifier: MIT
//! Fake players this plugin owns: spawn one, hold keys, act with its hands
//! and read its state. A fake is an online player to every other call
//! (`players::state`, `give`, `teleport_player`, events), addressed by
//! its uuid; the server removes all of a plugin's fakes when it unloads.

use crate::host;
use crate::types::{
    BlockPos, BreakTool, Direction, FakeAction, FakeDig, FakeError, FakeInput, FakeInteract,
    FakeSpec, FakeState, FakeUseOn, GameMode, Inventory, Rotation, Uuid, Vec3,
};

/// The keys and look a fake holds: `forward` and `strafe` are -1, 0 or 1.
pub type Input = FakeInput;

/// What to spawn; `Spawn::new("Bot").at(...).spawn()`.
#[derive(Clone, Debug)]
pub struct Spawn {
    spec: FakeSpec,
}

impl Spawn {
    pub fn new(name: &str) -> Self {
        Self {
            spec: FakeSpec {
                name: name.to_owned(),
                uuid: None,
                slim_skin: false,
                dimension: None,
                pos: None,
                rotation: None,
                game_mode: None,
                listed: true,
            },
        }
    }

    pub fn at(mut self, dimension: &str, x: f64, y: f64, z: f64) -> Self {
        self.spec.dimension = Some(dimension.to_owned());
        self.spec.pos = Some(Vec3 { x, y, z });
        self
    }

    pub fn facing(mut self, yaw: f32, pitch: f32) -> Self {
        self.spec.rotation = Some(Rotation { yaw, pitch });
        self
    }

    pub fn game_mode(mut self, mode: GameMode) -> Self {
        self.spec.game_mode = Some(mode);
        self
    }

    pub fn slim(mut self) -> Self {
        self.spec.slim_skin = true;
        self
    }

    /// Hides it from the tab list.
    pub fn unlisted(mut self) -> Self {
        self.spec.listed = false;
        self
    }

    pub fn uuid(mut self, uuid: Uuid) -> Self {
        self.spec.uuid = Some(uuid);
        self
    }

    pub fn spawn(self) -> Result<FakePlayer, FakeError> {
        host::fake_player_spawn(&self.spec).map(FakePlayer)
    }
}

/// A fake player this plugin owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FakePlayer(Uuid);

/// Every fake this plugin owns.
pub fn all() -> Vec<FakePlayer> {
    host::fake_players().into_iter().map(FakePlayer).collect()
}

impl FakePlayer {
    pub fn uuid(&self) -> Uuid {
        self.0
    }

    /// The uuid as 32 hex digits, for the `players` module.
    pub fn id(&self) -> String {
        self.0.to_string()
    }

    /// Removes it from the world.
    pub fn remove(self) -> Result<(), FakeError> {
        host::fake_player_remove(self.0)
    }

    /// Holds `input` until the next call; call on changes, not every tick.
    pub fn hold(&self, input: Input) -> Result<(), FakeError> {
        host::fake_player_input(self.0, input)
    }

    /// Holds the keys of `input` with the look unchanged from `state`.
    pub fn walk(&self, forward: i8, sprint: bool) -> Result<(), FakeError> {
        let look = self.state().map_or((0.0, 0.0), |s| (s.yaw, s.pitch));
        self.hold(Input {
            forward,
            strafe: 0,
            jump: false,
            sprint,
            sneak: false,
            yaw: look.0,
            pitch: look.1,
        })
    }

    /// Releases every key, keeping the look.
    pub fn stop(&self) -> Result<(), FakeError> {
        self.walk(0, false)
    }

    pub fn act(&self, action: FakeAction) -> Result<(), FakeError> {
        host::fake_player_act(self.0, action)
    }

    pub fn look(&self, yaw: f32, pitch: f32) -> Result<(), FakeError> {
        self.act(FakeAction::Look(Rotation { yaw, pitch }))
    }

    pub fn swing(&self) -> Result<(), FakeError> {
        self.act(FakeAction::Swing)
    }

    /// Hits the entity or player `target`.
    pub fn attack(&self, target: Uuid) -> Result<(), FakeError> {
        self.act(FakeAction::Attack(target))
    }

    pub fn interact(&self, entity: Uuid, off_hand: bool) -> Result<(), FakeError> {
        self.act(FakeAction::Interact(FakeInteract { entity, off_hand }))
    }

    /// Starts mining the block at `pos`, hit on `face`.
    pub fn dig(&self, pos: BlockPos, face: Direction) -> Result<(), FakeError> {
        self.act(FakeAction::Dig(FakeDig { pos, face }))
    }

    pub fn abort_dig(&self) -> Result<(), FakeError> {
        self.act(FakeAction::AbortDig)
    }

    pub fn use_item_on(
        &self,
        pos: BlockPos,
        face: Direction,
        cursor: Vec3,
        off_hand: bool,
    ) -> Result<(), FakeError> {
        self.act(FakeAction::UseItemOn(FakeUseOn {
            off_hand,
            pos,
            face,
            cursor,
        }))
    }

    pub fn use_item(&self, off_hand: bool) -> Result<(), FakeError> {
        self.act(FakeAction::UseItem(off_hand))
    }

    pub fn release_use(&self) -> Result<(), FakeError> {
        self.act(FakeAction::ReleaseUse)
    }

    /// Selects hotbar slot 0 to 8.
    pub fn select_slot(&self, slot: u8) -> Result<(), FakeError> {
        self.act(FakeAction::SelectSlot(slot))
    }

    pub fn swap_hands(&self) -> Result<(), FakeError> {
        self.act(FakeAction::SwapHands)
    }

    pub fn drop_item(&self, whole_stack: bool) -> Result<(), FakeError> {
        self.act(FakeAction::Drop(whole_stack))
    }

    pub fn respawn(&self) -> Result<(), FakeError> {
        self.act(FakeAction::Respawn)
    }

    /// Its state as of its last tick; `None` when it is gone.
    pub fn state(&self) -> Option<FakeState> {
        host::fake_player_state(self.0)
    }

    /// Its inventory as of its last tick, slots in menu numbering (36 to
    /// 44 the hotbar); `None` when it is gone.
    pub fn inventory(&self) -> Option<Inventory> {
        host::fake_player_inventory(self.0)
    }

    /// The hotbar slot that breaks `state` fastest and the ticks it takes;
    /// `None` when it cannot break it.
    pub fn break_ticks(&self, state: &str) -> Option<BreakTool> {
        host::fake_player_break_ticks(self.0, state)
    }
}
