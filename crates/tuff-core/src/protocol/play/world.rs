use bytes::Bytes;
use tuff_vanilla_data::packet::play::Clientbound as Id;
use tuff_vanilla_data::registry::SoundEvent;

use crate::protocol::codec::WriteExt;
use crate::protocol::{Encode, Packet};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_wire::PacketWriter;
use tuff_world_model::BlockPos;

#[derive(Packet)]
#[packet(id = Id::ChunkBatchStart, encode_only)]
pub struct ChunkBatchStart;

#[derive(Packet)]
#[packet(id = Id::ChangeDifficulty, encode_only)]
pub struct ChangeDifficulty {
    pub difficulty: u8,
    pub locked: bool,
}

/// The target tick rate and whether the server is frozen (`/tick`).
#[derive(Packet)]
#[packet(id = Id::TickingState, encode_only)]
pub struct SetTickingState {
    pub rate: f32,
    pub frozen: bool,
}

/// How many ticks are left to run while frozen (`/tick step`).
#[derive(Packet)]
#[packet(id = Id::TickingStep, encode_only)]
pub struct StepTick(#[packet(var_int)] pub i32);

pub mod game_event {
    pub const NO_RESPAWN_BLOCK_AVAILABLE: u8 = 0;
    pub const START_RAINING: u8 = 1;
    pub const STOP_RAINING: u8 = 2;
    pub const CHANGE_GAME_MODE: u8 = 3;
    pub const RAIN_LEVEL_CHANGE: u8 = 7;
    pub const THUNDER_LEVEL_CHANGE: u8 = 8;
    pub const PUFFER_FISH_STING: u8 = 9;
    pub const GUARDIAN_ELDER_EFFECT: u8 = 10;
    pub const IMMEDIATE_RESPAWN: u8 = 11;
    pub const LIMITED_CRAFTING: u8 = 12;
    pub const START_WAITING_FOR_CHUNKS: u8 = 13;
    /// Rolls the credits; closing them makes the client ask to respawn.
    pub const WIN_GAME: u8 = 4;
}

/// Boss Bar: one bar identified by `uuid`, added, changed or removed.
pub struct BossEvent<'a> {
    pub uuid: u128,
    pub action: BossAction<'a>,
}

pub enum BossAction<'a> {
    Add {
        title: &'a TextComponent,
        progress: f32,
        color: i32,
        overlay: i32,
        flags: u8,
    },
    Remove,
    Progress(f32),
    Title(&'a TextComponent),
    Style {
        color: i32,
        overlay: i32,
    },
    Flags(u8),
}

pub mod boss_flags {
    pub const DARKEN_SCREEN: u8 = 0x01;
    pub const PLAY_MUSIC: u8 = 0x02;
    pub const CREATE_FOG: u8 = 0x04;
}

impl Encode for BossEvent<'_> {
    const ID: i32 = Id::BossEvent.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_uuid(self.uuid);
        match &self.action {
            BossAction::Add {
                title,
                progress,
                color,
                overlay,
                flags,
            } => {
                w.write_var_int(0);
                w.write_text(title);
                w.write_float(*progress);
                w.write_var_int(*color);
                w.write_var_int(*overlay);
                w.write_unsigned_byte(*flags);
            }
            BossAction::Remove => w.write_var_int(1),
            BossAction::Progress(progress) => {
                w.write_var_int(2);
                w.write_float(*progress);
            }
            BossAction::Title(title) => {
                w.write_var_int(3);
                w.write_text(title);
            }
            BossAction::Style { color, overlay } => {
                w.write_var_int(4);
                w.write_var_int(*color);
                w.write_var_int(*overlay);
            }
            BossAction::Flags(flags) => {
                w.write_var_int(5);
                w.write_unsigned_byte(*flags);
            }
        }
    }
}

/// Debug Sample: one tick's timings in nanoseconds (full tick, tick
/// work, tasks, idle) for the client's tick chart.
pub struct DebugSample {
    pub sample: [i64; 4],
}

/// The sample type of server tick timings.
pub const TICK_TIME_SAMPLE: i32 = 0;

impl Encode for DebugSample {
    const ID: i32 = Id::DebugSample.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.sample.len() as i32);
        for value in self.sample {
            w.write_long(value);
        }
        w.write_var_int(TICK_TIME_SAMPLE);
    }
}

#[derive(Packet, Debug, Clone, Copy, PartialEq)]
#[packet(id = Id::GameEvent, encode_only)]
pub struct GameEvent {
    pub event: u8,
    pub value: f32,
}

#[derive(Packet)]
#[packet(id = Id::SetChunkCacheCenter, encode_only)]
pub struct SetChunkCacheCenter {
    #[packet(var_int)]
    pub x: i32,
    #[packet(var_int)]
    pub z: i32,
}

/// The server's view distance, which caps the client's own.
#[derive(Packet)]
#[packet(id = Id::SetChunkCacheRadius, encode_only)]
pub struct SetChunkCacheRadius(#[packet(var_int)] pub i32);

#[derive(Packet)]
#[packet(id = Id::SetSimulationDistance, encode_only)]
pub struct SetSimulationDistance(#[packet(var_int)] pub i32);

#[derive(Packet)]
#[packet(id = Id::ChunkBatchFinished, encode_only)]
pub struct ChunkBatchFinished(#[packet(var_int)] pub i32);

/// Unload Chunk. The client reads the position as one long with Z in the
/// upper half, hence the field order.
#[derive(Packet)]
#[packet(id = Id::ForgetLevelChunk, encode_only)]
pub struct ForgetLevelChunk {
    #[packet(int)]
    pub z: i32,
    #[packet(int)]
    pub x: i32,
}

/// Chunk Data and Update Light, with the body after the packet ID already
/// encoded by the world's chunk storage.
pub struct LevelChunkWithLight(pub Bytes);

impl Encode for LevelChunkWithLight {
    const ID: i32 = Id::LevelChunkWithLight.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_bytes(&self.0);
    }
}

/// Chunks Biomes for one chunk, with the biome data of every section
/// already encoded.
pub struct ChunksBiomes {
    pub x: i32,
    pub z: i32,
    pub data: Bytes,
}

impl Encode for ChunksBiomes {
    const ID: i32 = Id::ChunksBiomes.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(1);
        w.write_int(self.z);
        w.write_int(self.x);
        w.write_var_int(self.data.len() as i32);
        w.write_bytes(&self.data);
    }
}

/// Update Tags, with the body after the packet ID (as configuration's).
#[derive(Packet)]
#[packet(id = Id::UpdateTags, encode_only)]
pub struct UpdateTags(#[packet(rest)] pub Bytes);

/// Update Light, with the body after the packet ID (chunk X and Z as
/// VarInts, then the light data) already encoded by the world's chunk storage.
#[derive(Packet)]
#[packet(id = Id::LightUpdate, encode_only)]
pub struct LightUpdate(#[packet(rest)] pub Bytes);

pub struct InitializeBorder {
    pub center: [f64; 2],
    pub old_size: f64,
    pub new_size: f64,
    /// Ticks left of the size change.
    pub lerp_time: i64,
    pub absolute_max_size: i32,
    pub warning_blocks: i32,
    /// In ticks.
    pub warning_time: i32,
}

impl Encode for InitializeBorder {
    const ID: i32 = Id::InitializeBorder.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_double(self.center[0]);
        w.write_double(self.center[1]);
        w.write_double(self.old_size);
        w.write_double(self.new_size);
        w.write_var_long(self.lerp_time);
        w.write_var_int(self.absolute_max_size);
        w.write_var_int(self.warning_blocks);
        w.write_var_int(self.warning_time);
    }
}

pub struct SetBorderCenter {
    pub center: [f64; 2],
}

impl Encode for SetBorderCenter {
    const ID: i32 = Id::SetBorderCenter.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_double(self.center[0]);
        w.write_double(self.center[1]);
    }
}

pub struct SetBorderLerpSize {
    pub old_size: f64,
    pub new_size: f64,
    pub lerp_time: i64,
}

impl Encode for SetBorderLerpSize {
    const ID: i32 = Id::SetBorderLerpSize.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_double(self.old_size);
        w.write_double(self.new_size);
        w.write_var_long(self.lerp_time);
    }
}

pub struct SetBorderSize(pub f64);

impl Encode for SetBorderSize {
    const ID: i32 = Id::SetBorderSize.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_double(self.0);
    }
}

pub struct SetBorderWarningDelay(pub i32);

impl Encode for SetBorderWarningDelay {
    const ID: i32 = Id::SetBorderWarningDelay.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.0);
    }
}

pub struct SetBorderWarningDistance(pub i32);

impl Encode for SetBorderWarningDistance {
    const ID: i32 = Id::SetBorderWarningDistance.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.0);
    }
}

#[derive(Packet)]
#[packet(id = Id::SetDefaultSpawnPosition, encode_only)]
pub struct SetDefaultSpawnPosition<'a> {
    pub dimension: &'a str,
    pub location: BlockPos,
    pub yaw: f32,
    pub pitch: f32,
}

/// Every available game rule's value, as the game serializes it.
pub struct GameRuleValues<'a>(pub &'a [(String, String)]);

impl Encode for GameRuleValues<'_> {
    const ID: i32 = Id::GameRuleValues.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.0.len() as i32);
        for (rule, value) in self.0 {
            w.write_string(rule);
            w.write_string(value);
        }
    }
}

pub struct SetTime {
    pub world_age: i64,
    pub clocks: Vec<ClockTime>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockTime {
    /// `minecraft:world_clock` registry ID.
    pub clock: i32,
    pub total_ticks: i64,
    pub partial_tick: f32,
    /// Clock ticks per client tick; 0 while the clock is paused.
    pub rate: f32,
}

impl Encode for SetTime {
    const ID: i32 = Id::SetTime.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_long(self.world_age);
        w.write_var_int(self.clocks.len() as i32);
        for clock in &self.clocks {
            w.write_var_int(clock.clock);
            w.write_var_long(clock.total_ticks);
            w.write_float(clock.partial_tick);
            w.write_float(clock.rate);
        }
    }
}

#[derive(Packet)]
#[packet(id = Id::KeepAlive, encode_only)]
pub struct KeepAlive(#[packet(long)] pub i64);

/// Ping: the client answers with a Pong carrying the same id.
#[derive(Packet)]
#[packet(id = Id::Ping, encode_only)]
pub struct Ping(#[packet(int)] pub i32);

/// Block Entity Data: the client-visible NBT of a block entity (a
/// campfire's food, a sign's text).
pub struct BlockEntityData<'a> {
    pub position: BlockPos,
    pub kind: u32,
    pub nbt: &'a Compound,
}

impl Encode for BlockEntityData<'_> {
    const ID: i32 = Id::BlockEntityData.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_position(self.position);
        w.write_var_int(self.kind as i32);
        w.write_nbt(&Tag::Compound(self.nbt.clone()))
            .expect("in-memory NBT encodes");
    }
}

/// Open Sign Editor: the client opens the editor on one side of a sign.
pub struct OpenSignEditor {
    pub position: BlockPos,
    pub front: bool,
}

impl Encode for OpenSignEditor {
    const ID: i32 = Id::OpenSignEditor.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_position(self.position);
        w.write_bool(self.front);
    }
}

/// Open Book: the client opens the book in the given hand.
pub struct OpenBook {
    pub off_hand: bool,
}

impl Encode for OpenBook {
    const ID: i32 = Id::OpenBook.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(i32::from(self.off_hand));
    }
}

/// Show Dialog: the client opens an inline dialog (a holder with no
/// registry id, so the definition follows as NBT).
pub struct ShowDialog {
    pub dialog: Compound,
}

impl Encode for ShowDialog {
    const ID: i32 = Id::ShowDialog.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(0);
        w.write_nbt(&Tag::Compound(self.dialog.clone()))
            .expect("in-memory NBT encodes");
    }
}

/// Clear Dialog: closes any dialog the client shows.
/// Crash report detail map the client attaches to its reports.
pub struct CustomReportDetails<'a>(pub &'a [(&'a str, &'a str)]);

impl Encode for CustomReportDetails<'_> {
    const ID: i32 = Id::CustomReportDetails.id();

    fn encode(&self, w: &mut PacketWriter) {
        crate::protocol::custom::write_report_details(w, self.0)
            .expect("report details within vanilla limits");
    }
}

pub struct ClearDialog;

impl Encode for ClearDialog {
    const ID: i32 = Id::ClearDialog.id();

    fn encode(&self, _: &mut PacketWriter) {}
}

/// Show Dialog for a dialog the client already has in its registry.
pub struct ShowRegistryDialog {
    pub id: i32,
}

impl Encode for ShowRegistryDialog {
    const ID: i32 = Id::ShowDialog.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.id + 1);
    }
}

pub mod block_event {
    /// Note blocks: play the note (particle only; the sound is separate).
    pub const NOTE: u8 = 0;
    /// Chests and shulker boxes: the parameter is the number of viewers.
    pub const CONTAINER_OPEN_COUNT: u8 = 1;
    /// Bells: ring, the parameter being the struck direction.
    pub const BELL_RING: u8 = 1;
    /// Decorated pots: wobble, the parameter being 0 for an insertion and
    /// 1 for a refusal.
    pub const DECORATED_POT_WOBBLE: u8 = 1;
}

/// Block Event (Block Action): a block's non-persistent animation, such as
/// a chest lid; `block` is a `minecraft:block` registry id.
pub struct BlockEvent {
    pub position: BlockPos,
    pub action: u8,
    pub parameter: u8,
    pub block: u32,
}

impl Encode for BlockEvent {
    const ID: i32 = Id::BlockEvent.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_position(self.position);
        w.write_unsigned_byte(self.action);
        w.write_unsigned_byte(self.parameter);
        w.write_var_int(self.block as i32);
    }
}

pub struct BlockUpdate {
    pub position: BlockPos,
    pub state: u32,
}

impl Encode for BlockUpdate {
    const ID: i32 = Id::BlockUpdate.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_position(self.position);
        w.write_var_int(self.state as i32);
    }
}

/// Block changes within one chunk section, each `(x << 8 | z << 4 | y,
/// state)` relative to the section.
pub struct SectionBlocksUpdate<'a> {
    pub section: [i32; 3],
    pub blocks: &'a [(u16, u32)],
}

impl Encode for SectionBlocksUpdate<'_> {
    const ID: i32 = Id::SectionBlocksUpdate.id();

    fn encode(&self, w: &mut PacketWriter) {
        let [x, y, z] = self.section.map(i64::from);
        w.write_long(((x & 0x3F_FFFF) << 42) | ((z & 0x3F_FFFF) << 20) | (y & 0xF_FFFF));
        w.write_var_int(self.blocks.len() as i32);
        for &(index, state) in self.blocks {
            w.write_var_long((i64::from(state) << 12) | i64::from(index));
        }
    }
}

#[derive(Packet)]
#[packet(id = Id::BlockChangedAck, encode_only)]
pub struct BlockChangedAck(#[packet(var_int)] pub i32);

pub mod level_event {
    pub const SOUND_DRAGON_FIREBALL: i32 = 1017;
    pub const SOUND_BLAZE_FIREBALL: i32 = 1018;
    pub const SOUND_FIREWORK_SHOOT: i32 = 1004;
    pub const SOUND_WIND_CHARGE_SHOOT: i32 = 1051;
    pub const SOUND_DRAGON_DEATH: i32 = 1028;
    pub const SOUND_WITHER_BOSS_SPAWN: i32 = 1023;
    pub const SOUND_ANVIL_BROKEN: i32 = 1029;
    pub const SOUND_ANVIL_LAND: i32 = 1031;
    pub const PARTICLES_DRAGON_FIREBALL_SPLASH: i32 = 2006;
    pub const PARTICLES_DRAGON_BLOCK_BREAK: i32 = 2008;
    pub const ANIMATION_END_GATEWAY_SPAWN: i32 = 3000;
    pub const ANIMATION_DRAGON_SUMMON_ROAR: i32 = 3001;
    pub const SOUND_EXTINGUISH_FIRE: i32 = 1009;
    pub const SOUND_PLAY_JUKEBOX_SONG: i32 = 1010;
    pub const SOUND_STOP_JUKEBOX_SONG: i32 = 1011;
    pub const PARTICLES_CONSUME_EFFECT_TELEPORT: i32 = 2017;
    pub const PARTICLES_WAX_ON: i32 = 3003;
    pub const PARTICLES_WAX_OFF: i32 = 3004;
    pub const PARTICLES_SCRAPE: i32 = 3005;
    pub const SOUND_PORTAL_TRAVEL: i32 = 1032;
    pub const SOUND_BREWING_STAND_BREW: i32 = 1035;
    pub const SOUND_SPELL_POTION_SPLASH: i32 = 1053;
    pub const SOUND_INSTANT_POTION_SPLASH: i32 = 1054;
    pub const PARTICLES_SPELL_POTION_SPLASH: i32 = 2002;
    pub const PARTICLES_INSTANT_POTION_SPLASH: i32 = 2007;
    pub const SOUND_END_PORTAL_SPAWN: i32 = 1038;
    pub const LAVA_FIZZ: i32 = 1501;
    pub const END_PORTAL_FRAME_FILL: i32 = 1503;
    pub const BLOCK_BREAK: i32 = 2001;
    pub const PARTICLES_SMASH_ATTACK: i32 = 2013;
    pub const DESTROY_PROGRESS: i32 = 2019;
    pub const DESTROY_PROGRESS_SOUND: i32 = 2020;
    pub const ANIMATION_SCULK_SHRIEK: i32 = 3007;
    pub const PARTICLES_AND_SOUND_BRUSH_BLOCK_COMPLETE: i32 = 3008;
}

/// Another player's crack overlay on a block: stage 0-9, anything else
/// clears it.
pub struct BlockDestruction {
    pub id: i32,
    pub position: BlockPos,
    pub stage: i8,
}

impl Encode for BlockDestruction {
    const ID: i32 = Id::BlockDestruction.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.id);
        w.write_position(self.position);
        w.write_byte(self.stage);
    }
}

pub struct LevelEvent {
    pub event: i32,
    pub position: BlockPos,
    pub data: i32,
    /// Played at full volume wherever the client is.
    pub global: bool,
}

impl Encode for LevelEvent {
    const ID: i32 = Id::LevelEvent.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_int(self.event);
        w.write_position(self.position);
        w.write_int(self.data);
        w.write_bool(self.global);
    }
}

pub mod sound_category {
    pub const MASTER: i32 = 0;
    pub const MUSIC: i32 = 1;
    pub const RECORDS: i32 = 2;
    pub const WEATHER: i32 = 3;
    pub const BLOCKS: i32 = 4;
    pub const HOSTILE: i32 = 5;
    pub const NEUTRAL: i32 = 6;
    pub const PLAYERS: i32 = 7;
    pub const AMBIENT: i32 = 8;
    pub const VOICE: i32 = 9;
    pub const UI: i32 = 10;
}

pub struct Sound {
    pub sound_event: u32,
    pub category: i32,
    pub position: [f64; 3],
    pub volume: f32,
    pub pitch: f32,
    pub seed: i64,
}

impl Encode for Sound {
    const ID: i32 = Id::Sound.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.sound_event as i32 + 1);
        w.write_var_int(self.category);
        for c in self.position {
            w.write_int((c * 8.0) as i32);
        }
        w.write_float(self.volume);
        w.write_float(self.pitch);
        w.write_long(self.seed);
    }
}

/// A sound that follows an entity.
pub struct SoundEntity {
    pub sound_event: u32,
    pub category: i32,
    pub entity_id: i32,
    pub volume: f32,
    pub pitch: f32,
    pub seed: i64,
}

impl Encode for SoundEntity {
    const ID: i32 = Id::SoundEntity.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.sound_event as i32 + 1);
        w.write_var_int(self.category);
        w.write_var_int(self.entity_id);
        w.write_float(self.volume);
        w.write_float(self.pitch);
        w.write_long(self.seed);
    }
}

#[cfg(test)]
mod sound_entity_tests {
    use super::*;
    use crate::protocol::encode_packet;

    #[tuff_core::watchdog]
    #[test]
    fn sound_entity_writes_holder_source_entity_volume_pitch_seed() {
        let bytes = encode_packet(&SoundEntity {
            sound_event: 0,
            category: 2,
            entity_id: 300,
            volume: 1.0,
            pitch: 0.5,
            seed: 7,
        })
        .into_bytes();
        let body = &bytes[bytes.len() - 20..];
        assert_eq!(&body[..4], &[1, 2, 0xAC, 0x02]);
        assert_eq!(&body[4..8], &1.0f32.to_be_bytes());
        assert_eq!(&body[8..12], &0.5f32.to_be_bytes());
        assert_eq!(&body[12..], &7i64.to_be_bytes());
    }
}

/// Explosion: the blast's particles and sound, and the push this player
/// gets from it.
pub struct Explode<'a> {
    pub center: [f64; 3],
    pub radius: f32,
    pub block_count: i32,
    pub knockback: Option<[f64; 3]>,
    /// A particle type without data.
    pub particle: u32,
    pub sound: u32,
    /// `(particle, scaling, speed, weight)`, particles without data.
    pub block_particles: &'a [(u32, f32, f32, u32)],
}

impl Encode for Explode<'_> {
    const ID: i32 = Id::Explode.id();

    fn encode(&self, w: &mut PacketWriter) {
        for c in self.center {
            w.write_double(c);
        }
        w.write_float(self.radius);
        w.write_int(self.block_count);
        w.write_bool(self.knockback.is_some());
        if let Some(knockback) = self.knockback {
            for c in knockback {
                w.write_double(c);
            }
        }
        w.write_var_int(self.particle as i32);
        w.write_var_int(self.sound as i32 + 1);
        w.write_var_int(self.block_particles.len() as i32);
        for (particle, scaling, speed, weight) in self.block_particles {
            w.write_var_int(*particle as i32);
            w.write_float(*scaling);
            w.write_float(*speed);
            w.write_var_int(*weight as i32);
        }
        w.write_bool(true);
    }
}

/// Level Particles for a particle type that carries no data.
pub struct Particles {
    pub particle: u32,
    pub position: [f64; 3],
    pub spread: [f32; 3],
    pub speed: f32,
    pub count: i32,
    /// The game's alternative randomization, which spreads particles over
    /// the given distances rather than around a Gaussian.
    pub alternative: bool,
}

impl Encode for Particles {
    const ID: i32 = Id::LevelParticles.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.particle as i32);
        w.write_bool(false);
        w.write_bool(false);
        for c in self.position {
            w.write_double(c);
        }
        for d in self.spread {
            w.write_float(d);
        }
        for _ in 0..3 {
            w.write_float(self.speed);
        }
        w.write_var_int(self.count);
        w.write_var_int(if self.alternative { 1 } else { 0 });
    }
}

/// Level Particles for any particle type, with the option data the type
/// reads after its id (see `command::particle`).
pub struct ParticlesWithOptions<'a> {
    pub particle: u32,
    pub options: &'a [u8],
    pub force: bool,
    pub position: [f64; 3],
    pub spread: [f32; 3],
    pub speed: f32,
    pub count: i32,
}

impl Encode for ParticlesWithOptions<'_> {
    const ID: i32 = Id::LevelParticles.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.particle as i32);
        w.write_bytes(self.options);
        w.write_bool(self.force);
        w.write_bool(false);
        for c in self.position {
            w.write_double(c);
        }
        for d in self.spread {
            w.write_float(d);
        }
        for _ in 0..3 {
            w.write_float(self.speed);
        }
        w.write_var_int(self.count);
        w.write_var_int(0);
    }
}

/// A sound by name, for sounds outside the registry as well as in it.
pub struct NamedSound<'a> {
    pub sound: &'a str,
    pub category: i32,
    pub position: [f64; 3],
    pub volume: f32,
    pub pitch: f32,
    pub seed: i64,
}

impl Encode for NamedSound<'_> {
    const ID: i32 = Id::Sound.id();

    fn encode(&self, w: &mut PacketWriter) {
        match SoundEvent::from_name(self.sound) {
            Some(event) => w.write_var_int(event.id() as i32 + 1),
            None => {
                w.write_var_int(0);
                w.write_string(self.sound);
                w.write_bool(false);
            }
        }
        w.write_var_int(self.category);
        for c in self.position {
            w.write_int((c * 8.0) as i32);
        }
        w.write_float(self.volume);
        w.write_float(self.pitch);
        w.write_long(self.seed);
    }
}

pub struct StopSound<'a> {
    pub category: Option<i32>,
    pub sound: Option<&'a str>,
}

impl Encode for StopSound<'_> {
    const ID: i32 = Id::StopSound.id();

    fn encode(&self, w: &mut PacketWriter) {
        let flags = u8::from(self.category.is_some()) | u8::from(self.sound.is_some()) << 1;
        w.write_unsigned_byte(flags);
        if let Some(category) = self.category {
            w.write_var_int(category);
        }
        if let Some(sound) = self.sound {
            w.write_string(sound);
        }
    }
}

pub struct Transfer<'a> {
    pub host: &'a str,
    pub port: i32,
}

impl Encode for Transfer<'_> {
    const ID: i32 = Id::Transfer.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_string(self.host);
        w.write_var_int(self.port);
    }
}

/// One marker on a map, with the registry id of its decoration type.
#[derive(Debug, Clone, PartialEq)]
pub struct MapMarker {
    pub kind: u32,
    pub x: i8,
    pub y: i8,
    pub rotation: i8,
    pub name: Option<TextComponent>,
}

/// A rectangle of map colours, row by row.
#[derive(Debug, Clone, Copy)]
pub struct MapPatch<'a> {
    pub x: u8,
    pub y: u8,
    pub width: u8,
    pub height: u8,
    pub colors: &'a [u8],
}

/// Map Data: a map's markers and/or a patch of its pixels.
pub struct MapData<'a> {
    pub id: u32,
    pub scale: u8,
    pub locked: bool,
    pub markers: Option<&'a [MapMarker]>,
    pub patch: Option<MapPatch<'a>>,
}

impl Encode for MapData<'_> {
    const ID: i32 = Id::MapItemData.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.id as i32);
        w.write_byte(self.scale as i8);
        w.write_bool(self.locked);
        match self.markers {
            Some(markers) => {
                w.write_bool(true);
                w.write_var_int(markers.len() as i32);
                for marker in markers {
                    w.write_var_int(marker.kind as i32);
                    w.write_byte(marker.x);
                    w.write_byte(marker.y);
                    w.write_byte(marker.rotation);
                    match &marker.name {
                        Some(name) => {
                            w.write_bool(true);
                            w.write_text(name);
                        }
                        None => w.write_bool(false),
                    }
                }
            }
            None => w.write_bool(false),
        }
        match self.patch {
            Some(patch) => {
                w.write_byte(patch.width as i8);
                w.write_byte(patch.height as i8);
                w.write_byte(patch.x as i8);
                w.write_byte(patch.y as i8);
                w.write_var_int(patch.colors.len() as i32);
                w.write_bytes(patch.colors);
            }
            None => w.write_byte(0),
        }
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::*;
    use crate::protocol::encode_packet;

    #[tuff_core::watchdog]
    #[test]
    fn transfer_writes_host_then_port() {
        let bytes = encode_packet(&Transfer {
            host: "a.b",
            port: 25565,
        })
        .into_bytes();
        let id_len = encode_packet(&Transfer { host: "", port: 0 })
            .into_bytes()
            .len()
            - 2;
        assert_eq!(&bytes[id_len..], &[3, b'a', b'.', b'b', 0xDD, 0xC7, 0x01]);
    }
}

/// A debug feed's value, written after its `debug_subscription` id.
#[derive(Debug, Clone, PartialEq)]
pub enum DebugValue {
    Poi {
        pos: BlockPos,
        kind: i32,
        free_tickets: i32,
    },
    Unit,
    Pos(BlockPos),
    GameEvent {
        event: i32,
        pos: [f64; 3],
    },
    Structures(Vec<DebugStructure>),
    Path(Box<DebugPath>),
    Goals(Vec<DebugGoal>),
    Brain(Box<DebugBrain>),
    Bee {
        hive: Option<BlockPos>,
        flower: Option<BlockPos>,
        travel_ticks: i32,
        blacklisted_hives: Vec<BlockPos>,
    },
    Hive {
        block: i32,
        occupants: i32,
        honey_level: i32,
        sedated: bool,
    },
    Breeze {
        attack_target: Option<i32>,
        jump_target: Option<BlockPos>,
    },
    /// 0 in a block, 1 in a fluid, 2 in air.
    BlockIntersection(i32),
    GameEventListener {
        radius: i32,
    },
    /// A redstone wire orientation's id.
    Orientation(u8),
}

/// A block-aligned box, inclusive on both corners.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DebugBox {
    pub min: BlockPos,
    pub max: BlockPos,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DebugStructure {
    pub bounds: DebugBox,
    /// Each piece's box; the first is the start piece.
    pub pieces: Vec<DebugBox>,
}

/// A path node as the client draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DebugNode {
    pub pos: [i32; 3],
    pub walked_distance: f32,
    pub cost_malus: f32,
    pub closed: bool,
    pub path_type: i32,
    pub f: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DebugPath {
    pub reached: bool,
    pub next_node: i32,
    pub target: BlockPos,
    pub nodes: Vec<DebugNode>,
    pub open_set: Vec<DebugNode>,
    pub closed_set: Vec<DebugNode>,
    /// Never empty: the client rejects a path without targets.
    pub targets: Vec<DebugNode>,
    pub max_node_distance: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugGoal {
    pub priority: i32,
    pub running: bool,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DebugBrain {
    pub name: String,
    pub profession: String,
    pub xp: i32,
    pub health: f32,
    pub max_health: f32,
    pub inventory: String,
    pub wants_golem: bool,
    pub anger_level: i32,
    pub activities: Vec<String>,
    pub behaviors: Vec<String>,
    pub memories: Vec<String>,
    pub gossips: Vec<String>,
    pub pois: Vec<BlockPos>,
    pub potential_pois: Vec<BlockPos>,
}

fn write_box(w: &mut PacketWriter, b: &DebugBox) {
    w.write_position(b.min);
    w.write_position(b.max);
}

fn write_node(w: &mut PacketWriter, n: &DebugNode) {
    for v in n.pos {
        w.write_int(v);
    }
    w.write_float(n.walked_distance);
    w.write_float(n.cost_malus);
    w.write_bool(n.closed);
    w.write_var_int(n.path_type);
    w.write_float(n.f);
}

fn write_list<T>(w: &mut PacketWriter, items: &[T], mut each: impl FnMut(&mut PacketWriter, &T)) {
    w.write_var_int(items.len() as i32);
    for item in items {
        each(w, item);
    }
}

fn write_opt_pos(w: &mut PacketWriter, pos: Option<BlockPos>) {
    w.write_bool(pos.is_some());
    if let Some(pos) = pos {
        w.write_position(pos);
    }
}

fn write_strings(w: &mut PacketWriter, items: &[String]) {
    write_list(w, items, |w, s| w.write_string(s));
}

fn write_positions(w: &mut PacketWriter, items: &[BlockPos]) {
    write_list(w, items, |w, p| w.write_position(*p));
}

impl DebugValue {
    fn encode(&self, w: &mut PacketWriter) {
        match self {
            Self::Poi {
                pos,
                kind,
                free_tickets,
            } => {
                w.write_position(*pos);
                w.write_var_int(*kind);
                w.write_var_int(*free_tickets);
            }
            Self::Unit => {}
            Self::Pos(pos) => w.write_position(*pos),
            Self::GameEvent { event, pos } => {
                w.write_var_int(*event);
                for v in pos {
                    w.write_double(*v);
                }
            }
            Self::Structures(structures) => write_list(w, structures, |w, s| {
                write_box(w, &s.bounds);
                w.write_var_int(s.pieces.len() as i32);
                for (i, piece) in s.pieces.iter().enumerate() {
                    write_box(w, piece);
                    w.write_bool(i == 0);
                }
            }),
            Self::Path(path) => {
                w.write_bool(path.reached);
                w.write_int(path.next_node);
                w.write_position(path.target);
                write_list(w, &path.nodes, write_node);
                write_list(w, &path.open_set, write_node);
                write_list(w, &path.closed_set, write_node);
                write_list(w, &path.targets, write_node);
                w.write_float(path.max_node_distance);
            }
            Self::Goals(goals) => write_list(w, goals, |w, g| {
                w.write_var_int(g.priority);
                w.write_bool(g.running);
                w.write_string(&g.name);
            }),
            Self::Brain(b) => {
                w.write_string(&b.name);
                w.write_string(&b.profession);
                w.write_int(b.xp);
                w.write_float(b.health);
                w.write_float(b.max_health);
                w.write_string(&b.inventory);
                w.write_bool(b.wants_golem);
                w.write_int(b.anger_level);
                write_strings(w, &b.activities);
                write_strings(w, &b.behaviors);
                write_strings(w, &b.memories);
                write_strings(w, &b.gossips);
                write_positions(w, &b.pois);
                write_positions(w, &b.potential_pois);
            }
            Self::Bee {
                hive,
                flower,
                travel_ticks,
                blacklisted_hives,
            } => {
                write_opt_pos(w, *hive);
                write_opt_pos(w, *flower);
                w.write_var_int(*travel_ticks);
                write_positions(w, blacklisted_hives);
            }
            Self::Hive {
                block,
                occupants,
                honey_level,
                sedated,
            } => {
                w.write_var_int(*block);
                w.write_var_int(*occupants);
                w.write_var_int(*honey_level);
                w.write_bool(*sedated);
            }
            Self::Breeze {
                attack_target,
                jump_target,
            } => {
                w.write_bool(attack_target.is_some());
                if let Some(id) = attack_target {
                    w.write_var_int(*id);
                }
                write_opt_pos(w, *jump_target);
            }
            Self::BlockIntersection(kind) => w.write_var_int(*kind),
            Self::GameEventListener { radius } => w.write_var_int(*radius),
            Self::Orientation(index) => w.write_var_int(i32::from(*index)),
        }
    }
}

/// A feed's current value for one target, or `None` to clear it.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugUpdate {
    pub subscription: i32,
    pub value: Option<DebugValue>,
}

impl DebugUpdate {
    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.subscription);
        w.write_bool(self.value.is_some());
        if let Some(value) = &self.value {
            value.encode(w);
        }
    }
}

/// Debug Block Value: a feed's value at a block.
pub struct DebugBlockValue {
    pub pos: BlockPos,
    pub update: DebugUpdate,
}

impl Encode for DebugBlockValue {
    const ID: i32 = Id::DebugBlockValue.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_position(self.pos);
        self.update.encode(w);
    }
}

/// Debug Chunk Value: a feed's value for a chunk column.
pub struct DebugChunkValue {
    pub x: i32,
    pub z: i32,
    pub update: DebugUpdate,
}

impl Encode for DebugChunkValue {
    const ID: i32 = Id::DebugChunkValue.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_long(i64::from(self.x as u32) | (i64::from(self.z as u32) << 32));
        self.update.encode(w);
    }
}

/// Debug Entity Value: a feed's value for an entity.
pub struct DebugEntityValue {
    pub entity: i32,
    pub update: DebugUpdate,
}

impl Encode for DebugEntityValue {
    const ID: i32 = Id::DebugEntityValue.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity);
        self.update.encode(w);
    }
}

/// Debug Event: a one-off feed event the client shows for a while.
pub struct DebugEvent {
    pub subscription: i32,
    pub value: DebugValue,
}

impl Encode for DebugEvent {
    const ID: i32 = Id::DebugEvent.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.subscription);
        self.value.encode(w);
    }
}
