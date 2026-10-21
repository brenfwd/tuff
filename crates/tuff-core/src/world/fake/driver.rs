//! Runs a fake player's client movement each session tick: held keys go
//! through the motion hook and come out as the packets a client sends, in
//! its order.

use bytes::Bytes;
use tokio::sync::{mpsc, watch};

use super::motion::{Body, Held, MotionState};
use super::{BodyStatus, Observed};
use crate::protocol::play::{Movement, Serverbound};
use crate::world::session::PlayerSession;

/// A move this small is not worth a packet, unless 20 ticks passed.
const SILENT_SQ: f64 = 2.0e-4 * 2.0e-4;
const REMINDER_TICKS: u32 = 20;
const START_SPRINTING: i32 = 1;
const STOP_SPRINTING: i32 = 2;
const START_FALL_FLYING: i32 = 6;

pub struct Driver {
    inputs: mpsc::UnboundedReceiver<Held>,
    pub(super) actions: mpsc::UnboundedReceiver<super::actions::FakeAction>,
    pub(super) dig: Option<super::actions::Dig>,
    /// An attack waiting for its target to come into view, with the ticks
    /// it still waits.
    pub(super) pending_attack: Option<(i32, u8)>,
    pub(super) sequence: i32,
    outbound: mpsc::UnboundedReceiver<Bytes>,
    observed: watch::Sender<Observed>,
    pub(super) held: Held,
    state: Option<MotionState>,
    teleports: u64,
    impulses: u64,
    sent_flags: u8,
    sent_sprint: bool,
    sent_gliding: bool,
    sent_position: [f64; 3],
    pub(super) sent_rotation: (f32, f32),
    pub(super) sent_ground: bool,
    reminder: u32,
}

impl Driver {
    pub fn new(
        inputs: mpsc::UnboundedReceiver<Held>,
        actions: mpsc::UnboundedReceiver<super::actions::FakeAction>,
        outbound: mpsc::UnboundedReceiver<Bytes>,
        observed: watch::Sender<Observed>,
    ) -> Self {
        Self {
            inputs,
            actions,
            dig: None,
            pending_attack: None,
            sequence: 0,
            outbound,
            observed,
            held: Held::default(),
            state: None,
            teleports: 0,
            impulses: 0,
            sent_flags: 0,
            sent_sprint: false,
            sent_gliding: false,
            sent_position: [0.0; 3],
            sent_rotation: (0.0, 0.0),
            sent_ground: true,
            reminder: 0,
        }
    }
}

impl PlayerSession {
    /// One client tick of a fake player. Does nothing for a real one.
    pub(crate) async fn drive_fake(&mut self) {
        let Some(mut driver) = self.fake_driver.take() else {
            return;
        };
        let mut answers = Vec::new();
        while let Ok(frames) = driver.outbound.try_recv() {
            answers.extend(super::client::answer(&frames, self.id, &driver.observed));
        }
        for packet in answers {
            self.on_packet(packet).await;
        }
        let acted = self.fake_actions(&mut driver);
        for packet in acted {
            self.on_packet(packet).await;
        }
        if let Some(motion) = self.shared.fake_motion.clone() {
            let packets = self.fake_tick(&mut driver, &*motion);
            for packet in packets {
                self.on_packet(packet).await;
            }
        }
        self.publish_fake_body(&mut driver);
        self.fake_driver = Some(driver);
    }

    fn publish_fake_body(&self, driver: &mut Driver) {
        let status = BodyStatus {
            position: driver.state.as_ref().map_or([0.0; 3], |s| s.position),
            rotation: (driver.held.yaw, driver.held.pitch),
            dimension: Some(self.dimension),
            velocity: driver.state.as_ref().map_or([0.0; 3], |s| s.velocity),
            on_ground: driver.state.as_ref().is_some_and(|s| s.on_ground),
            in_fluid: driver.state.as_ref().is_some_and(|s| s.in_water),
            collided: driver.state.as_ref().is_some_and(|s| s.collided),
            health: self.health(),
            food: self.body.food.level,
            selected_slot: self.inventory.selected() as u8,
            hotbar: std::array::from_fn(|i| {
                self.inventory.slots()[crate::world::inventory::Inventory::hotbar_slot(i)]
                    .as_ref()
                    .map(|stack| stack.item)
            }),
            dig_progress: driver.dig_progress(),
            alive: !self.is_dead(),
            items: self
                .inventory
                .slots()
                .iter()
                .enumerate()
                .filter_map(|(slot, stack)| {
                    Some((
                        slot as u8,
                        tuff_item::item::ItemSnapshot::of(stack.as_ref()?),
                    ))
                })
                .collect(),
            miner: Some(self.miner()),
        };
        driver.observed.send_if_modified(|observed| {
            let changed = observed.body != status;
            observed.body = status;
            changed
        });
    }

    fn fake_tick(
        &mut self,
        driver: &mut Driver,
        motion: &dyn super::motion::FakeMotion,
    ) -> Vec<Serverbound> {
        while let Ok(held) = driver.inputs.try_recv() {
            driver.held = held;
        }
        let observed = driver.observed.borrow().clone();
        if observed.teleports == 0 || self.is_dead() {
            return Vec::new();
        }
        let state = driver.state.get_or_insert_with(|| {
            driver.teleports = observed.teleports;
            driver.impulses = observed.impulses;
            driver.sent_position = observed.position;
            driver.sent_rotation = observed.rotation;
            let mut state = MotionState::new(observed.position);
            state.velocity = observed.velocity;
            state
        });
        if observed.teleports != driver.teleports {
            driver.teleports = observed.teleports;
            driver.impulses = observed.impulses;
            state.position = observed.position;
            state.velocity = observed.velocity;
            state.on_ground = false;
            driver.sent_position = observed.position;
            driver.sent_rotation = observed.rotation;
        } else if observed.impulses != driver.impulses {
            driver.impulses = observed.impulses;
            state.velocity = observed.velocity;
        }
        let held = driver.held;
        let body = Body {
            movement: crate::world::physics::Movement::read(|a| self.attribute_value(a)),
            food: self.body.food.level,
            glider: self
                .inventory
                .equipped(tuff_vanilla_data::item::EquipmentSlot::Chest)
                .is_some_and(tuff_item::item::ItemStack::can_glide),
            boosting: self.motion.firework_boost_ticks > 0,
            fluids: &*self.shared.fluid_behaviors,
        };
        motion.tick(state, &held, &body, self.client_world());

        let mut out = Vec::new();
        let flags = held.flags();
        if flags != driver.sent_flags {
            driver.sent_flags = flags;
            out.push(Serverbound::PlayerInput(flags));
        }
        if state.sprinting != driver.sent_sprint {
            driver.sent_sprint = state.sprinting;
            out.push(Serverbound::PlayerCommand(if state.sprinting {
                START_SPRINTING
            } else {
                STOP_SPRINTING
            }));
        }
        if state.gliding != driver.sent_gliding {
            driver.sent_gliding = state.gliding;
            if state.gliding {
                out.push(Serverbound::PlayerCommand(START_FALL_FLYING));
            }
        }
        driver.reminder += 1;
        let moved: f64 = (0..3)
            .map(|axis| (state.position[axis] - driver.sent_position[axis]).powi(2))
            .sum();
        let position =
            (moved > SILENT_SQ || driver.reminder >= REMINDER_TICKS).then_some(state.position);
        let rotation =
            ((held.yaw, held.pitch) != driver.sent_rotation).then_some((held.yaw, held.pitch));
        if position.is_some() || rotation.is_some() || state.on_ground != driver.sent_ground {
            if position.is_some() {
                driver.sent_position = state.position;
                driver.reminder = 0;
            }
            if let Some(rotation) = rotation {
                driver.sent_rotation = rotation;
            }
            driver.sent_ground = state.on_ground;
            out.push(Serverbound::Move(Movement {
                position,
                rotation,
                on_ground: state.on_ground,
                horizontal_collision: false,
            }));
        }
        out.push(Serverbound::ClientTickEnd);
        out
    }
}
