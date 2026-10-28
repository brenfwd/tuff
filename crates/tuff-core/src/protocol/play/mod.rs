//! Play-state packets. Clientbound packets only implement encoding;
//! serverbound packets are decoded through [`Serverbound::decode`].
//!
//! Split by responsibility: [`entity`] (spawn/track/metadata/attribute
//! packets), [`world`] (chunk/block/sound/level-event packets),
//! [`inventory`] (container/menu/recipe-book packets), [`player`]
//! (login/respawn/abilities/player-info/effect/title packets), [`chat`]
//! (commands/chat/advancements packets) and [`serverbound`] (the single
//! serverbound decode enum). Everything is re-exported here so callers use
//! `crate::protocol::play::Whatever` regardless of which file it lives in.

mod chat;
mod entity;
mod inventory;
mod player;
mod scoreboard;
mod serverbound;
mod world;

pub use chat::*;
pub use entity::*;
pub use inventory::*;
pub use player::*;
pub use scoreboard::*;
pub use serverbound::*;
pub use world::*;
#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use tuff_vanilla_data::packet::play::{Clientbound as Id, Serverbound as ServerId};

    use super::*;
    use crate::protocol::codec::WriteExt;
    use crate::protocol::encode_packet;
    use tuff_wire::{Buffer, PacketReader, PacketWriter};
    use tuff_world_model::BlockPos;

    fn payload(w: PacketWriter) -> Bytes {
        let mut buf = Buffer::new();
        buf.extend(&w.finish()).unwrap();
        buf.try_split_packet().unwrap().unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn sound_encodes_pitch_float_after_volume() {
        let packet = Sound {
            sound_event: 4,
            category: sound_category::NEUTRAL,
            position: [1.0, 2.0, 3.0],
            volume: 0.5,
            pitch: 1.25,
            seed: 7,
        };
        let bytes = payload(encode_packet(&packet));
        let n = bytes.len();
        assert_eq!(bytes[n - 8 - 4..n - 8], 1.25f32.to_be_bytes());
        assert_eq!(bytes[n - 8 - 8..n - 8 - 4], 0.5f32.to_be_bytes());
    }

    #[tuff_core::watchdog]
    #[test]
    fn set_ticking_state_writes_rate_then_frozen() {
        let packet = SetTickingState {
            rate: 10.0,
            frozen: true,
        };
        let mut r = PacketReader::new(payload(encode_packet(&packet)));
        assert_eq!(r.read_var_int().unwrap(), Id::TickingState.id());
        assert_eq!(r.read_float().unwrap(), 10.0);
        assert!(r.read_bool().unwrap());
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn map_data_writes_markers_then_patch() {
        let markers = [MapMarker {
            kind: 0,
            x: -128,
            y: 127,
            rotation: 3,
            name: None,
        }];
        let colors = [4u8, 5, 6, 7];
        let packet = MapData {
            id: 300,
            scale: 2,
            locked: true,
            markers: Some(&markers),
            patch: Some(MapPatch {
                x: 1,
                y: 2,
                width: 2,
                height: 2,
                colors: &colors,
            }),
        };
        let mut r = PacketReader::new(payload(encode_packet(&packet)));
        assert_eq!(r.read_var_int().unwrap(), Id::MapItemData.id());
        assert_eq!(r.read_var_int().unwrap(), 300);
        assert_eq!(r.read_byte().unwrap(), 2);
        assert!(r.read_bool().unwrap());
        assert!(r.read_bool().unwrap());
        assert_eq!(r.read_var_int().unwrap(), 1);
        assert_eq!(r.read_var_int().unwrap(), 0);
        assert_eq!(r.read_byte().unwrap(), -128);
        assert_eq!(r.read_byte().unwrap(), 127);
        assert_eq!(r.read_byte().unwrap(), 3);
        assert!(!r.read_bool().unwrap());
        assert_eq!(r.read_byte().unwrap(), 2);
        assert_eq!(r.read_byte().unwrap(), 2);
        assert_eq!(r.read_byte().unwrap(), 1);
        assert_eq!(r.read_byte().unwrap(), 2);
        assert_eq!(r.read_var_int().unwrap(), 4);
        assert_eq!(r.remaining(), 4);
    }

    #[tuff_core::watchdog]
    #[test]
    fn map_data_without_a_patch_ends_with_a_zero_width() {
        let packet = MapData {
            id: 1,
            scale: 0,
            locked: false,
            markers: None,
            patch: None,
        };
        let mut r = PacketReader::new(payload(encode_packet(&packet)));
        r.read_var_int().unwrap();
        r.read_var_int().unwrap();
        r.read_byte().unwrap();
        assert!(!r.read_bool().unwrap());
        assert!(!r.read_bool().unwrap());
        assert_eq!(r.read_byte().unwrap(), 0);
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn step_tick_writes_a_var_int_step_count() {
        let packet = StepTick(5);
        let mut r = PacketReader::new(payload(encode_packet(&packet)));
        assert_eq!(r.read_var_int().unwrap(), Id::TickingStep.id());
        assert_eq!(r.read_var_int().unwrap(), 5);
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn game_event_ids_match_protocol() {
        assert_eq!(game_event::START_RAINING, 1);
        assert_eq!(game_event::STOP_RAINING, 2);
        assert_eq!(game_event::CHANGE_GAME_MODE, 3);
        assert_eq!(game_event::RAIN_LEVEL_CHANGE, 7);
        assert_eq!(game_event::THUNDER_LEVEL_CHANGE, 8);
        assert_eq!(game_event::IMMEDIATE_RESPAWN, 11);
        assert_eq!(game_event::LIMITED_CRAFTING, 12);
        assert_eq!(game_event::START_WAITING_FOR_CHUNKS, 13);

        let packet = GameEvent {
            event: game_event::LIMITED_CRAFTING,
            value: 0.0,
        };
        let mut r = PacketReader::new(payload(encode_packet(&packet)));
        assert_eq!(r.read_var_int().unwrap(), Id::GameEvent.id());
        assert_eq!(r.read_unsigned_byte().unwrap(), 12);
        assert_eq!(r.read_float().unwrap(), 0.0);
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn section_blocks_update_packs_the_section_and_entries() {
        let packet = SectionBlocksUpdate {
            section: [-2, -4, 3],
            blocks: &[(0x5A3, 1), (0xFFF, 30_000)],
        };
        let mut r = PacketReader::new(payload(encode_packet(&packet)));
        assert_eq!(r.read_var_int().unwrap(), Id::SectionBlocksUpdate.id());
        let section = r.read_long().unwrap();
        assert_eq!(section >> 42, -2);
        assert_eq!(section << 44 >> 44, -4);
        assert_eq!(section << 22 >> 42, 3);
        assert_eq!(r.read_var_int().unwrap(), 2);
        assert_eq!(r.read_var_long().unwrap(), (1 << 12) | 0x5A3);
        assert_eq!(r.read_var_long().unwrap(), (30_000 << 12) | 0xFFF);
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn decodes_movement_and_rejects_trailing_bytes() {
        let mut w = PacketWriter::new();
        w.write_var_int(ServerId::MovePlayerPosRot.id());
        w.write_double(1.5);
        w.write_double(72.0);
        w.write_double(-3.25);
        w.write_float(90.0);
        w.write_float(10.0);
        w.write_byte(1);
        assert_eq!(
            Serverbound::decode(payload(w)).unwrap(),
            Serverbound::Move(Movement {
                position: Some([1.5, 72.0, -3.25]),
                rotation: Some((90.0, 10.0)),
                on_ground: true,
                horizontal_collision: false,
            })
        );

        let mut w = PacketWriter::new();
        w.write_var_int(ServerId::Attack.id());
        w.write_var_int(7);
        w.write_byte(0);
        assert!(Serverbound::decode(payload(w)).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn custom_payload_decodes_and_caps_its_size() {
        let build = |len: usize| {
            let mut w = PacketWriter::new();
            w.write_var_int(ServerId::CustomPayload.id());
            w.write_string("tuff:test");
            w.write_bytes(&vec![5; len]);
            payload(w)
        };
        assert_eq!(
            Serverbound::decode(build(3)).unwrap(),
            Serverbound::CustomPayload {
                channel: "tuff:test".into(),
                data: bytes::Bytes::from_static(&[5, 5, 5]),
            }
        );
        assert!(Serverbound::decode(build(32767)).is_ok());
        assert!(Serverbound::decode(build(32768)).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn rejects_oversized_chat_and_click_lists() {
        let mut w = PacketWriter::new();
        w.write_var_int(ServerId::Chat.id());
        w.write_string(&"a".repeat(2000));
        assert!(Serverbound::decode(payload(w)).is_err());

        let mut w = PacketWriter::new();
        w.write_var_int(ServerId::ContainerClick.id());
        w.write_var_int(0);
        w.write_var_int(0);
        w.write_short(0);
        w.write_byte(0);
        w.write_var_int(0);
        w.write_var_int(10_000);
        assert!(Serverbound::decode(payload(w)).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn equipment_sets_continuation_bit_on_all_but_last() {
        let packet = SetEquipment {
            entity_id: 1,
            items: &[
                (equipment_slot::MAIN_HAND, None),
                (equipment_slot::OFF_HAND, None),
            ],
            obfuscate: false,
        };
        let mut r = PacketReader::new(payload(encode_packet(&packet)));
        assert_eq!(r.read_var_int().unwrap(), Id::SetEquipment.id());
        assert_eq!(r.read_var_int().unwrap(), 1);
        assert_eq!(r.read_unsigned_byte().unwrap(), 0x80);
        assert_eq!(r.read_var_int().unwrap(), 0);
        assert_eq!(r.read_unsigned_byte().unwrap(), 0x01);
        assert_eq!(r.read_var_int().unwrap(), 0);
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn player_action_status_roundtrip_and_rejects_invalid() {
        let expected = [
            (0, PlayerActionStatus::StartDestroyBlock),
            (1, PlayerActionStatus::ChangeDestroyDirection),
            (2, PlayerActionStatus::AbortDestroyBlock),
            (3, PlayerActionStatus::StopDestroyBlock),
            (4, PlayerActionStatus::DropAllItems),
            (5, PlayerActionStatus::DropItem),
            (6, PlayerActionStatus::ReleaseUseItem),
            (7, PlayerActionStatus::SwapItemWithOffhand),
            (8, PlayerActionStatus::Stab),
        ];

        for (id, status) in expected {
            assert_eq!(PlayerActionStatus::from_id(id).unwrap(), status);
            assert_eq!(status.id(), id);

            let mut w = PacketWriter::new();
            w.write_var_int(ServerId::PlayerAction.id());
            w.write_var_int(id);
            w.write_position(BlockPos::new(10, 64, -20));
            w.write_byte(2);
            w.write_var_int(42);

            let decoded = Serverbound::decode(payload(w)).unwrap();
            assert_eq!(
                decoded,
                Serverbound::PlayerAction {
                    status,
                    position: BlockPos::new(10, 64, -20),
                    face: 2,
                    sequence: 42,
                }
            );
        }

        assert!(PlayerActionStatus::from_id(-1).is_err());
        assert!(PlayerActionStatus::from_id(9).is_err());

        let mut w = PacketWriter::new();
        w.write_var_int(ServerId::PlayerAction.id());
        w.write_var_int(9);
        w.write_position(BlockPos::new(0, 0, 0));
        w.write_byte(0);
        w.write_var_int(0);
        assert!(Serverbound::decode(payload(w)).is_err());
    }
}
