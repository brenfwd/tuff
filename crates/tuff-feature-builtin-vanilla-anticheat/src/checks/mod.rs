//! The checks. Each sees every serverbound packet with the session facts
//! that held before it and may flag it. World-dependent checks (reach
//! with line of sight, placing against air, face validity) are meant to
//! plug in here once a [`Facts`] can carry a view of the player's blocks.

mod actions;
pub mod aim;
mod breaking;
pub mod budget;
mod chat;
mod combat;
pub mod engine;
pub mod fast_break;
mod flight;
mod ground;
mod integrity;
mod inventory;
mod item_use;
mod movement;
pub mod placement;
pub mod predict;
mod rates;
mod reach;
mod rotation;
mod sanity;
mod sight;
mod state;
mod vehicle;
mod vehicle_moves;
pub mod velocity;

use tuff_core::protocol::play::{GameMode, Serverbound};

use crate::trace::Trace;

pub use actions::{ContainerReach, EditLimits, NoSlow};
pub use aim::{AimCheck, AimTarget};
pub use breaking::BlockBreak;
pub use chat::{Echoes, Spam, echo_key};
pub use combat::{AttackSwing, SelfHit};
pub use engine::{Body, Engine, Nearby, Predicted, Prediction};
pub use fast_break::{FastBreak, Mining};
pub use flight::FlightEnvelope;
pub use ground::GroundSupport;
pub use integrity::{ClickBefore, ItemIntegrity, Tamper, tampered};
pub use inventory::{CreativeSlots, InventoryValidity};
pub use item_use::{ItemUse, ItemUseTimeline};
pub use movement::{AirBound, MoveEnvelope, SustainedSpeed, Terrain, TimerBalance};
pub use placement::{PlaceValidity, Placement, Scaffold};
pub use rates::rate_checks;
pub use reach::{Aim, Reach, ReachCheck};
pub use rotation::SnapTurn;
pub use sanity::Sanity;
pub use state::{GroundSpoof, KeyMismatch, PacketOrder, SneakSpoof, SprintLegal, SprintSpoof};
pub use vehicle::{BoatGravity, MountGravity, Vehicle};
pub(crate) use vehicle::{LERP_TICKS, bounds as entity_bounds, solid_entity};
pub use vehicle_moves::{VehicleControl, VehiclePhase, VehicleSpeed, VehicleTimer};

/// A check's name and violation ladder defaults.
#[derive(Debug, PartialEq)]
pub struct Info {
    pub name: &'static str,
    /// A level above this kicks.
    pub kick_above: f64,
    /// Level lost per session tick.
    pub decay: f64,
}

const fn info(name: &'static str, kick_above: f64, decay: f64) -> Info {
    Info {
        name,
        kick_above,
        decay,
    }
}

pub const INFOS: &[Info] = &[
    info("sanity", 40.0, 0.02),
    info("inventory", 30.0, 0.05),
    info("creative_slot", 30.0, 0.05),
    info("stack_count", 20.0, 0.05),
    info("item_data", 20.0, 0.05),
    info("dupe", 20.0, 0.05),
    info("attack_rate", 12.0, 0.1),
    info("swing_rate", 20.0, 0.1),
    info("click_rate", 15.0, 0.1),
    info("drop_rate", 15.0, 0.1),
    info("hotbar_rate", 15.0, 0.1),
    info("place_rate", 12.0, 0.1),
    info("use_rate", 15.0, 0.1),
    info("break_rate", 12.0, 0.1),
    info("chat_rate", 4.0, 0.02),
    info("command_rate", 4.0, 0.02),
    info("chat_spam", 10.0, 0.01),
    info("command_spam", 10.0, 0.01),
    info("flood", 30.0, 0.05),
    info("sprint", 20.0, 0.05),
    info("sneak", 20.0, 0.05),
    info("ground", 20.0, 0.1),
    info("order", 20.0, 0.05),
    info("move_speed", 25.0, 0.05),
    info("predict", 25.0, 0.05),
    info("fly", 25.0, 0.05),
    info("phase", 20.0, 0.05),
    info("timer", 30.0, 0.05),
    info("container", 20.0, 0.05),
    info("no_slow", 25.0, 0.05),
    info("edit", 15.0, 0.05),
    info("air_break", 12.0, 0.05),
    info("break_face", 12.0, 0.05),
    info("wrong_block", 12.0, 0.02),
    info("no_swing", 15.0, 0.05),
    info("nuker", 12.0, 0.05),
    info("reach", 15.0, 0.05),
    info("sight", 15.0, 0.05),
    info("place_air", 12.0, 0.05),
    info("place_face", 12.0, 0.05),
    info("place_wall", 15.0, 0.05),
    info("place_aim", 15.0, 0.05),
    info("scaffold", 12.0, 0.05),
    info("fast_bow", 12.0, 0.05),
    info("release_rate", 15.0, 0.1),
    info("use_attack", 12.0, 0.05),
    info("container_attack", 12.0, 0.05),
    info("attack_swing", 15.0, 0.05),
    info("self_hit", 10.0, 0.05),
    info("vehicle_fly", 25.0, 0.05),
    info("vehicle_speed", 25.0, 0.05),
    info("vehicle_phase", 20.0, 0.05),
    info("vehicle_timer", 30.0, 0.05),
    info("vehicle_control", 25.0, 0.05),
    info("velocity", 20.0, 0.05),
    info("improbable", 15.0, 0.05),
    info("aim", 15.0, 0.05),
    info("multi_aura", 12.0, 0.05),
    info("fast_break", 12.0, 0.05),
    info("rotation", 15.0, 0.05),
];

pub fn info_of(name: &str) -> Option<&'static Info> {
    INFOS.iter().find(|info| info.name == name)
}

/// What a session looked like when the packet arrived.
#[derive(Clone, Debug)]
pub struct Facts {
    pub tick: u64,
    pub game_mode: GameMode,
    pub instabuild: bool,
    pub may_fly: bool,
    pub sneaking: bool,
    /// Leather boots on the feet, which stand on powder snow.
    pub snow_walker: bool,
    pub sprinting: bool,
    pub food: i32,
    pub riding: bool,
    pub position: [f64; 3],
    pub on_ground: bool,
    pub teleport_pending: bool,
    /// This move is the position a teleport confirmation carried, not one
    /// of the client's ticks.
    pub confirming: bool,
    /// The open block menu's window id and container slot count.
    pub menu: Option<(i32, usize)>,
    pub now: std::time::Instant,
    pub latency_ms: i32,
    /// The server's target tick rate.
    pub tick_rate: f32,
    pub flying: bool,
    /// The flying speed the client was told (0.05 by default).
    pub fly_speed: f32,
    /// The top of the dimension's build range.
    pub ceiling: f64,
    /// Knockback, a blast, a push, gliding or a riptide gave the player
    /// velocity the position steps do not account for.
    pub velocity: bool,
    /// Extra motion a server push lets this move carry.
    pub push: velocity::Push,
    /// Velocities the server sent the client that may not have shown up
    /// yet, oldest first.
    pub impulses: Vec<velocity::Impulse>,
    /// Displacements the client applied to itself, with their sequence
    /// numbers.
    pub shoves: Vec<(u64, tuff_core::world::session::Shove)>,
    /// Elytra flight, as the server has it.
    pub gliding: bool,
    /// A firework attached to the player may be pulling it.
    pub firework: bool,
    /// A riptide spin.
    pub riptide: bool,
    /// Recently moved by something the velocity ledger does not see (a
    /// piston).
    pub shoved: bool,
    pub speed: Option<u8>,
    /// Jump Boost amplifier plus one, or zero.
    pub jump_boost: u8,
    pub levitation: bool,
    /// Levitation amplifier plus one, or zero.
    pub levitation_level: u8,
    pub slow_falling: bool,
    /// The Weaving effect, which cobwebs hold back less.
    pub weaving: bool,
    pub dolphins_grace: bool,
    /// The dimension makes lava flow fast (and push harder).
    pub fast_lava: bool,
    /// The client's blocks around a position move, when the packet has one.
    pub terrain: Option<Terrain>,
    /// Terrain at the current position for a Move without one (rotation or
    /// status only), so its ground claim can still be judged.
    pub resting: Option<Terrain>,
    /// Distance from the eyes to the nearest block of the open menu.
    pub menu_distance: Option<f64>,
    /// An item is being used (eaten, drawn, blocked with).
    pub using: bool,
    pub item_use: Option<ItemUse>,
    /// Command blocks may be edited: creative with operator rights.
    pub may_edit_commands: bool,
    /// Shortly after login or a confirmed teleport, when movement limits
    /// are relaxed.
    pub grace: bool,
    pub eye: [f64; 3],
    /// The client's block at a block action's position, when known.
    pub target: Option<tuff_vanilla_data::BlockState>,
    /// Where an attack, interaction, break or placement aims.
    pub reach: Option<Reach>,
    /// The clicked block and the view, for a `UseItemOn`.
    pub placement: Option<Placement>,
    /// The vehicle a rider's Move Vehicle moves.
    pub vehicle: Option<Vehicle>,
    /// The touched slots before a Click Container.
    pub click: Option<ClickBefore>,
    /// Yaw and pitch the server last accepted.
    pub rotation: [f32; 2],
    /// An attacked entity's recent hitboxes and the recent eyes.
    pub aim: Option<AimTarget>,
    /// How fast the player mines the block a start or stop names.
    pub mining: Option<Mining>,
    /// Other players' copies of this chat message in the last half minute.
    pub echoes: u32,
    /// The client's movement attributes.
    pub body: Body,
    /// Visible entities near a position move.
    pub nearby: Vec<engine::Nearby>,
    /// How the prediction engine judged a position move.
    pub prediction: Option<Prediction>,
    /// The player's own entity id.
    pub entity_id: i32,
}

/// How a flagged packet is undone.
#[derive(Clone, Debug, PartialEq)]
pub enum Fix {
    /// Drop the packet.
    Cancel,
    /// Count the violation but let the packet through.
    Pass,
    /// Drop it and resend the inventory.
    ResyncInventory,
    /// Drop it and teleport the player back to this position.
    Teleport([f64; 3]),
    /// Pass this packet on instead.
    Correct(Box<Serverbound>),
    /// Not yet a violation: stop the server's item use and tell the client.
    ResyncUse,
    /// Drop a vehicle move and put the client's vehicle back here; the
    /// client's moves count again once it confirms.
    MoveVehicle {
        position: [f64; 3],
        yaw: f32,
        pitch: f32,
    },
    /// Drop a vehicle move from a rider that does not steer and show its
    /// client the vehicle here; no confirmation follows.
    ResetVehicle {
        position: [f64; 3],
        yaw: f32,
        pitch: f32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Flag {
    pub check: &'static str,
    pub weight: f64,
    pub fix: Fix,
}

impl Flag {
    pub fn cancel(check: &'static str, weight: f64) -> Option<Flag> {
        Some(Flag {
            check,
            weight,
            fix: Fix::Cancel,
        })
    }

    pub fn resync(check: &'static str, weight: f64) -> Option<Flag> {
        Some(Flag {
            check,
            weight,
            fix: Fix::ResyncInventory,
        })
    }
}

/// One rule over the packet stream, with its own per-player state.
pub trait Check: Send + Sync {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag>;

    /// What the last examined packet was allowed and did, when this check
    /// judged it.
    fn trace(&self) -> Option<Trace> {
        None
    }

    /// Weight of a sub-threshold signal from the last examined packet,
    /// taken once; only feeder checks report one.
    fn weak(&mut self) -> f64 {
        0.0
    }
}

/// A fresh set of checks for one player.
pub fn defaults() -> Vec<Box<dyn Check>> {
    let mut all: Vec<Box<dyn Check>> = vec![
        Box::new(Sanity),
        Box::new(InventoryValidity),
        Box::new(CreativeSlots),
        Box::new(ItemIntegrity),
        Box::new(SprintSpoof::default()),
        Box::new(SneakSpoof::default()),
        Box::new(SprintLegal::default()),
        Box::new(KeyMismatch::default()),
        Box::new(GroundSpoof::default()),
        Box::new(PacketOrder),
        Box::new(TimerBalance::default()),
        Box::new(MoveEnvelope::default()),
        Box::new(SustainedSpeed::default()),
        Box::new(AirBound::default()),
        Box::new(FlightEnvelope::default()),
        Box::new(Predicted::default()),
        Box::new(ContainerReach),
        Box::new(NoSlow::default()),
        Box::new(ItemUseTimeline::default()),
        Box::new(EditLimits::default()),
        Box::new(Spam::chat()),
        Box::new(Spam::commands()),
        Box::new(BlockBreak::default()),
        Box::new(FastBreak::default()),
        Box::new(ReachCheck::default()),
        Box::new(AttackSwing::default()),
        Box::new(SelfHit),
        Box::new(AimCheck::default()),
        Box::new(SnapTurn::default()),
        Box::new(PlaceValidity::default()),
        Box::new(Scaffold::default()),
        Box::new(GroundSupport::default()),
        Box::new(BoatGravity::default()),
        Box::new(MountGravity::default()),
        Box::new(VehicleSpeed::default()),
        Box::new(VehiclePhase::default()),
        Box::new(VehicleTimer::default()),
        Box::new(VehicleControl::default()),
    ];
    all.extend(rate_checks());
    all
}

#[cfg(test)]
pub(crate) fn facts() -> Facts {
    Facts {
        tick: 0,
        game_mode: GameMode::Survival,
        instabuild: false,
        may_fly: false,
        sneaking: false,
        snow_walker: false,
        sprinting: false,
        food: 20,
        riding: false,
        position: [0.0, 64.0, 0.0],
        on_ground: true,
        teleport_pending: false,
        confirming: false,
        menu: None,
        now: std::time::Instant::now(),
        latency_ms: 0,
        tick_rate: 20.0,
        flying: false,
        velocity: false,
        push: velocity::Push::default(),
        impulses: Vec::new(),
        shoves: Vec::new(),
        gliding: false,
        firework: false,
        riptide: false,
        shoved: false,
        speed: None,
        jump_boost: 0,
        levitation: false,
        levitation_level: 0,
        slow_falling: false,
        weaving: false,
        dolphins_grace: false,
        fast_lava: false,
        fly_speed: 0.05,
        ceiling: 320.0,
        terrain: None,
        resting: None,
        menu_distance: None,
        using: false,
        item_use: None,
        may_edit_commands: true,
        grace: false,
        eye: [0.0, 65.62, 0.0],
        target: None,
        reach: None,
        placement: None,
        vehicle: None,
        click: None,
        rotation: [0.0, 0.0],
        aim: None,
        mining: None,
        echoes: 0,
        body: Body::default(),
        nearby: Vec::new(),
        prediction: None,
        entity_id: 1,
    }
}
