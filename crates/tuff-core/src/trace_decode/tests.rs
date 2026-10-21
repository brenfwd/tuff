use std::collections::HashSet;

use tuff_vanilla_data::Item;

use super::*;
use crate::protocol::Encode;
use crate::protocol::configuration as cfg;
use crate::protocol::pack;
use crate::protocol::play::*;
use crate::world::scoreboard::{NumberFormat, RenderType, Team};
use tuff_item::item::{ItemStack, MintReason};
use tuff_wire::PacketWriter;
use tuff_world_model::BlockPos;

fn encoded<P: Encode>(packet: &P) -> Bytes {
    let mut w = PacketWriter::new();
    w.write_var_int(P::ID);
    packet.encode(&mut w);
    w.into_bytes()
}

fn decoded<P: Encode>(state: ConnState, packet: &P) -> String {
    let bytes = encoded(packet);
    let mut r = PacketReader::new(bytes.clone());
    let id = r.read_var_int().unwrap();
    let text = match fixed(state, id, &mut r) {
        Some(text) => text.expect("fixed decoder"),
        None => {
            let (name, fields) = schema(state, id).expect("schema");
            decode_schema(name, fields, r).expect("schema decodes")
        }
    };
    assert!(!text.contains("trailing"), "{text}");
    assert!(!text.contains('\n'), "{text}");
    text
}

fn play<P: Encode>(packet: &P) -> String {
    let text = decoded(ConnState::Play, packet);
    assert!(!text.contains("..<"), "{text}");
    text
}

fn has(text: &str, parts: &[&str]) {
    for part in parts {
        assert!(text.contains(part), "{part:?} missing from {text}");
    }
}

#[tuff_core::watchdog]
#[test]
fn long_debug_is_capped_with_the_byte_count() {
    let text = cap("x".repeat(2000), 9000);
    assert!(text.len() < 600);
    assert!(text.ends_with("... (9000 bytes)"));
    assert_eq!(cap("short".into(), 5), "short");
}

#[tuff_core::watchdog]
#[test]
fn unknown_or_truncated_packets_are_not_typed() {
    assert!(typed_text(ConnState::Play, &Bytes::from_static(&[0x7f, 1, 2])).is_none());
    let damage = p::play::Clientbound::DamageEvent.id() as u8;
    assert!(typed_text(ConnState::Play, &Bytes::from(vec![damage, 1])).is_none());
    let passengers = p::play::Clientbound::SetPassengers.id() as u8;
    assert!(typed_text(ConnState::Play, &Bytes::from(vec![passengers, 1, 100])).is_none());
}

#[tuff_core::watchdog]
#[test]
fn every_clientbound_packet_is_decoded_or_listed_as_skipped() {
    let mut seen = HashSet::new();
    for (kind, _) in CONFIGURATION {
        assert!(seen.insert(*kind), "{kind:?} listed twice");
        assert!(!CONFIGURATION_SKIPPED.contains(kind), "{kind:?} both");
    }
    for kind in p::configuration::Clientbound::ALL {
        assert!(
            seen.contains(&kind) || CONFIGURATION_SKIPPED.contains(&kind),
            "configuration {kind:?} has no decoder and is not skipped"
        );
    }
    let mut seen = HashSet::new();
    for (kind, _) in PLAY {
        assert!(seen.insert(*kind), "{kind:?} listed twice");
        assert!(!PLAY_SKIPPED.contains(kind), "{kind:?} both");
    }
    for kind in p::play::Clientbound::ALL {
        assert!(
            seen.contains(&kind) || PLAY_SKIPPED.contains(&kind),
            "play {kind:?} has no decoder and is not skipped"
        );
    }
}

fn encoded_ids(source: &str, default: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for marker in ["const ID: i32 = ", "#[packet(id = "] {
        for (at, _) in source.match_indices(marker) {
            let path: String = source[at + marker.len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == ':' || *c == '_')
                .collect();
            let Some(name) = path.rsplit("::").next() else {
                continue;
            };
            let state = if path.contains("login::") || path.contains("status::") {
                continue;
            } else if path.contains("cfg::") || path.contains("configuration::") {
                "configuration"
            } else if path.contains("play::") {
                "play"
            } else if path.starts_with("Id::") || path.starts_with("Clientbound::") {
                default
            } else {
                continue;
            };
            out.push((state.to_owned(), name.to_owned()));
        }
    }
    out
}

#[tuff_core::watchdog]
#[test]
fn every_encoded_clientbound_packet_has_a_decoder() {
    let sources = [
        (include_str!("../protocol/play/chat.rs"), "play"),
        (include_str!("../protocol/play/entity.rs"), "play"),
        (include_str!("../protocol/play/inventory.rs"), "play"),
        (include_str!("../protocol/play/player.rs"), "play"),
        (include_str!("../protocol/play/scoreboard.rs"), "play"),
        (include_str!("../protocol/play/world.rs"), "play"),
        (
            include_str!("../protocol/configuration.rs"),
            "configuration",
        ),
        (include_str!("../protocol/pack.rs"), "none"),
    ];
    let mut checked = 0;
    for (source, default) in sources {
        for (state, name) in encoded_ids(source, default) {
            let covered = match state.as_str() {
                "play" => PLAY.iter().any(|(k, _)| format!("{k:?}") == name),
                "configuration" => CONFIGURATION.iter().any(|(k, _)| format!("{k:?}") == name),
                _ => continue,
            };
            assert!(covered, "{state} {name} is encoded but has no decoder");
            checked += 1;
        }
    }
    assert!(checked > 90, "only {checked} encoders found");
}

#[tuff_core::watchdog]
#[test]
fn ping_teleport_entity_and_positioned_damage_decode() {
    assert_eq!(play(&Ping(77)), "Ping { id: 77 }");
    let text = play(&TeleportEntity {
        entity_id: 4,
        position: [1.0, 2.0, 3.0],
        velocity: [0.0; 3],
        yaw: 90.0,
        pitch: 0.0,
        relative: relative::Y_ROT | relative::X_ROT,
        on_ground: true,
    });
    has(&text, &["entity_id: 4", "relative: 24", "on_ground: true"]);
    let text = play(&DamageEvent {
        entity_id: 7,
        damage_type: 0,
        cause: None,
        direct: Some(3),
        source_position: Some([1.0, 2.0, 3.0]),
    });
    has(&text, &["direct: 3", "source_position: [1.0, 2.0, 3.0]"]);
}

#[tuff_core::watchdog]
#[test]
fn damage_event_and_system_chat_decode_to_typed_text() {
    let text = play(&DamageEvent {
        entity_id: 7,
        damage_type: tuff_vanilla_data::data_registry::DAMAGE_TYPE
            .entries
            .binary_search(&"minecraft:fall")
            .unwrap() as i32,
        cause: Some(9),
        direct: Some(9),
        source_position: None,
    });
    assert_eq!(
        text,
        "DamageEvent { entity_id: 7, damage_type: minecraft:fall, cause: 9, direct: 9, \
         source_position: None }"
    );
    let content = TextComponent::new("hello");
    let text = play(&SystemChat {
        content: &content,
        overlay: true,
    });
    assert_eq!(text, "SystemChat { content: \"hello\", overlay: true }");
}

#[tuff_core::watchdog]
#[test]
fn entity_movement_and_riding_packets_round_trip() {
    has(
        &play(&AddEntity {
            entity_id: 5,
            uuid: 0xabc,
            kind: tuff_vanilla_data::registry::EntityType::OakBoat.id() as i32,
            position: [1.5, 64.0, -2.0],
            velocity: [0.0, 0.0, 0.0],
            pitch: 0.0,
            yaw: 90.0,
            head_yaw: 0.0,
            data: 0,
        }),
        &[
            "AddEntity { entity_id: 5",
            "kind: minecraft:oak_boat",
            "[1.5, 64.0, -2.0]",
            "yaw: 90.0",
        ],
    );
    has(
        &play(&EntityPositionSync {
            entity_id: 3,
            position: [1.0, 2.0, 3.0],
            yaw: 45.0,
            pitch: -10.0,
            on_ground: true,
        }),
        &["position: [1.0, 2.0, 3.0], yaw: 45.0, pitch: -10.0, on_ground: true"],
    );
    assert_eq!(
        play(&SetPassengers {
            vehicle: 9,
            passengers: &[1, 2],
        }),
        "SetPassengers { vehicle: 9, passengers: [1, 2] }"
    );
    assert_eq!(
        play(&MoveVehicle {
            position: [0.5, 70.0, 0.5],
            yaw: 1.0,
            pitch: 2.0,
        }),
        "MoveVehicle { position: [0.5, 70.0, 0.5], yaw: 1.0, pitch: 2.0 }"
    );
    has(
        &play(&SetEntityMotion {
            entity_id: 4,
            velocity: [0.0, 0.0, 0.0],
        }),
        &["SetEntityMotion { entity_id: 4, velocity: [0.0, 0.0, 0.0] }"],
    );
    has(
        &play(&PlayerPosition {
            teleport_id: 11,
            position: [1.0, 2.0, 3.0],
            yaw: 4.0,
            pitch: 5.0,
        }),
        &["teleport_id: 11", "relative: 0"],
    );
    has(
        &play(&RotateHead {
            entity_id: 1,
            head_yaw: 180.0,
        }),
        &["head_yaw: 180.0"],
    );
    has(&play(&RemoveEntities(&[1, 2, 3])), &["ids: [1, 2, 3]"]);
    has(
        &play(&EntityEvent {
            entity_id: 2,
            status: 3,
        }),
        &["EntityEvent { entity_id: 2, status: 3 }"],
    );
    has(
        &play(&Animate {
            entity_id: 1,
            animation: 1,
        }),
        &["animation: 1"],
    );
    has(&play(&SwingAnimation::whack(1, 0)), &["duration: 6"]);
    has(
        &play(&TakeItemEntity {
            collected: 1,
            collector: 2,
            count: 3,
        }),
        &["count: 3"],
    );
}

#[tuff_core::watchdog]
#[test]
fn player_state_packets_round_trip() {
    has(
        &play(&Login {
            entity_id: 1,
            dimensions: &["minecraft:overworld"],
            max_players: 20,
            view_distance: 10,
            simulation_distance: 8,
            dimension_type: 0,
            dimension: "minecraft:overworld",
            game_mode: GameMode::Creative,
            previous_game_mode: None,
            sea_level: 63,
            online_mode: true,
            reduced_debug_info: true,
            hardcore: true,
            last_death: None,
            is_debug: false,
            is_flat: true,
            portal_cooldown: 5,
        }),
        &[
            "dimension_type: minecraft:overworld",
            "reduced_debug_info: true",
            "game_mode: creative",
            "previous_game_mode: None",
            "sea_level: 63, online_mode: true",
            "flat: true",
            "portal_cooldown: 5",
        ],
    );
    has(
        &play(&Respawn {
            dimension_type: 2,
            dimension: "minecraft:the_end",
            game_mode: GameMode::Survival,
            previous_game_mode: Some(GameMode::Creative),
            sea_level: 0,
            data_kept: 1,
            last_death: None,
            is_debug: true,
            is_flat: false,
            portal_cooldown: 0,
        }),
        &[
            "previous_game_mode: creative",
            "data_kept: 1",
            "debug: true",
        ],
    );
    assert_eq!(
        play(&SetHealth {
            health: 20.0,
            food: 18,
            saturation: 5.0,
        }),
        "SetHealth { health: 20.0, food: 18, saturation: 5.0 }"
    );
    has(
        &play(&GameEvent {
            event: game_event::CHANGE_GAME_MODE,
            value: 1.0,
        }),
        &["event: change_game_mode, value: 1.0"],
    );
    has(
        &play(&PlayerAbilities {
            flags: 2,
            flying_speed: 0.05,
            fov_modifier: 0.1,
        }),
        &["flags: 2"],
    );
    has(
        &play(&SetExperience {
            progress: 0.5,
            level: 3,
            total: 30,
        }),
        &["level: 3"],
    );
    has(&play(&SetHeldSlot(4)), &["slot: 4"]);
    let effect = tuff_vanilla_data::registry::MobEffect::Speed.id() as i32;
    has(
        &play(&UpdateMobEffect {
            entity_id: 1,
            effect,
            amplifier: 1,
            duration: 100,
            flags: 2,
        }),
        &["effect: minecraft:speed"],
    );
    has(
        &play(&RemoveMobEffect {
            entity_id: 1,
            effect,
        }),
        &["effect: minecraft:speed"],
    );
    has(
        &play(&Cooldown {
            group: "minecraft:ender_pearl",
            ticks: 20,
        }),
        &["ticks: 20"],
    );
    let text = TextComponent::new("Title");
    has(&play(&SetTitleText(&text)), &["\"Title\""]);
    has(&play(&SetSubtitleText(&text)), &["\"Title\""]);
    has(&play(&SetActionBarText(&text)), &["\"Title\""]);
    has(
        &play(&SetTitlesAnimation {
            fade_in: 1,
            stay: 2,
            fade_out: 3,
        }),
        &["fade_out: 3"],
    );
    has(&play(&ClearTitles { reset: true }), &["reset: true"]);
    has(
        &play(&PlayerCombatKill {
            player_id: 1,
            message: &text,
        }),
        &["message: \"Title\""],
    );
    let entry = PlayerInfoEntry {
        uuid: 1,
        name: "Steve".into(),
        properties: vec![],
        game_mode: GameMode::Adventure,
        listed: true,
        latency: 42,
        hat: true,
        display_name: Some(text.clone()),
        list_order: 7,
        chat_mode: 0,
    };
    has(
        &play(&PlayerInfoUpdate {
            actions: player_info::ADD_PLAYER
                | player_info::UPDATE_GAME_MODE
                | player_info::UPDATE_LISTED
                | player_info::UPDATE_LATENCY
                | player_info::UPDATE_DISPLAY_NAME
                | player_info::UPDATE_LIST_ORDER
                | player_info::UPDATE_HAT,
            entries: std::slice::from_ref(&entry),
        }),
        &[
            "name: \"Steve\"",
            "game_mode: adventure",
            "latency: 42",
            "display_name: \"Title\"",
            "list_order: 7",
            "hat: true",
        ],
    );
    has(&play(&PlayerInfoRemove(&[1, 2])), &["uuids: ["]);
    has(&play(&Disconnect(&text)), &["reason: \"Title\""]);
}

#[tuff_core::watchdog]
#[test]
fn metadata_attributes_and_equipment_round_trip() {
    let stack = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    let mut damaged = ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test);
    damaged.set_damage(5);
    has(
        &play(&SetEntityData {
            entity_id: 8,
            values: &[
                (0, MetadataValue::Byte(0x20)),
                (9, MetadataValue::Float(4.5)),
                (
                    2,
                    MetadataValue::OptionalText(Some(TextComponent::new("Bob"))),
                ),
                (
                    8,
                    MetadataValue::Slot(Some(tuff_item::item::ItemSnapshot::of(&stack))),
                ),
                (6, MetadataValue::Pose(0)),
                (13, MetadataValue::OptionalVarInt(Some(4))),
                (
                    14,
                    MetadataValue::OptionalPosition(Some(BlockPos::new(1, 2, 3))),
                ),
                (15, MetadataValue::Boolean(true)),
                (16, MetadataValue::Position(BlockPos::new(4, 5, 6))),
                (
                    17,
                    MetadataValue::OptionalGlobalPosition(Some((
                        "minecraft:the_end".into(),
                        BlockPos::new(7, 8, 9),
                    ))),
                ),
            ],
        }),
        &[
            "values: [0=32, 9=4.5, 2=\"Bob\", 8=minecraft:diamond_sword x1, 6=0, 13=4, \
           14=(1, 2, 3), 15=true, 16=(4, 5, 6), \
           17={ dimension: \"minecraft:the_end\", pos: (7, 8, 9) }]",
        ],
    );
    has(
        &play(&SetEquipment {
            entity_id: 1,
            items: &[
                (equipment_slot::MAIN_HAND, Some(damaged.view())),
                (equipment_slot::HEAD, None),
            ],
        }),
        &[
            "items: [0=minecraft:iron_pickaxe x1 +[\"minecraft:damage\"]",
            "5=empty]",
        ],
    );
    has(
        &play(&UpdateAttributes {
            entity_id: 1,
            properties: &[AttributeProperty {
                attribute: tuff_vanilla_data::registry::Attribute::MovementSpeed,
                base: 0.1,
                modifiers: &[AttributeModifierData {
                    id: "minecraft:sprinting",
                    amount: 0.3,
                    operation: tuff_vanilla_data::item::ModifierOperation::AddMultipliedTotal,
                }],
            }],
        }),
        &[
            "attribute: minecraft:movement_speed",
            "id: \"minecraft:sprinting\"",
        ],
    );
    has(&play(&AwardStats(&[(1, 2, 3)])), &["stats: [[1, 2, 3]]"]);
}

#[tuff_core::watchdog]
#[test]
fn container_packets_round_trip() {
    let stack = ItemStack::mint(Item::Stone, 3, MintReason::Test);
    has(
        &play(&ContainerSetContent {
            window_id: 1,
            state_id: 2,
            slots: &[Some(stack.dup_unchecked()), None],
            carried: None,
        }),
        &["slots: [minecraft:stone x3, empty], carried: empty"],
    );
    has(
        &play(&ContainerSetSlot {
            window_id: 0,
            state_id: 1,
            slot: 36,
            item: Some(stack.view()),
        }),
        &["slot: 36, item: minecraft:stone x3"],
    );
    has(&play(&SetCursorItem(None)), &["item: empty"]);
    has(
        &play(&SetPlayerInventory {
            slot: 5,
            item: Some(stack.view()),
        }),
        &["slot: 5, item: minecraft:stone x3"],
    );
    has(&play(&StartConfiguration), &["StartConfiguration"]);
    has(
        &play(&ProjectilePower {
            entity_id: 7,
            acceleration_power: 0.5,
        }),
        &["entity_id: 7", "power: 0.5"],
    );
    has(&play(&ContainerClose(1)), &["window_id: 1"]);
    has(
        &play(&ContainerSetData {
            window_id: 1,
            property: 2,
            value: 3,
        }),
        &["value: 3"],
    );
    let title = TextComponent::new("Chest");
    has(
        &play(&OpenScreen {
            window_id: 1,
            menu_type: tuff_vanilla_data::registry::Menu::Generic9x3.id(),
            title: &title,
        }),
        &["menu: minecraft:generic_9x3", "title: \"Chest\""],
    );
    has(&play(&RecipeBookRemove(&[1, 2])), &["ids: [1, 2]"]);
    has(
        &play(&RecipeBookSettings([(true, false); 4])),
        &["books: [[true, false]"],
    );
    let offer = crate::world::trading::MerchantOffer {
        buy: crate::world::trading::ItemCost {
            item: Item::Emerald,
            count: 2,
            components: Default::default(),
        },
        buy_b: None,
        sell: ItemStack::mint(Item::Bread, 4, MintReason::Test),
        uses: 0,
        max_uses: 12,
        reward_exp: true,
        special_price: 0,
        demand: 0,
        price_multiplier: 0.05,
        xp: 1,
    };
    has(
        &play(&MerchantOffers {
            window_id: 3,
            offers: std::slice::from_ref(&offer),
            level: 1,
            xp: 0,
            regular: true,
            can_restock: true,
        }),
        &[
            "buy: minecraft:emerald x2",
            "sell: minecraft:bread x4",
            "max_uses: 12",
        ],
    );
}

#[tuff_core::watchdog]
#[test]
fn world_packets_round_trip() {
    let pos = BlockPos::new(1, -2, 3);
    let stone = tuff_vanilla_data::block::BlockState::from_id(1).unwrap();
    has(
        &play(&BlockUpdate {
            position: pos,
            state: stone.id(),
        }),
        &["BlockUpdate { position: (1, -2, 3), state: minecraft:stone }"],
    );
    has(
        &play(&SectionBlocksUpdate {
            section: [-1, 4, 2],
            blocks: &[((3 << 8) | (5 << 4) | 7, stone.id())],
        }),
        &["(-1, 4, 2), blocks: [(3, 7, 5)=minecraft:stone]"],
    );
    has(&play(&BlockChangedAck(5)), &["sequence: 5"]);
    has(
        &play(&DebugSample {
            sample: [1, 2, 3, 4],
        }),
        &["DebugSample", "[1, 2, 3, 4]"],
    );
    has(
        &play(&DebugBlockValue {
            pos,
            update: DebugUpdate {
                subscription: 8,
                value: Some(DebugValue::Poi {
                    pos,
                    kind: 0,
                    free_tickets: 1,
                }),
            },
        }),
        &["DebugBlockValue", "pois", "free_tickets: 1"],
    );
    has(
        &play(&DebugEvent {
            subscription: 14,
            value: DebugValue::Pos(pos),
        }),
        &["DebugEvent", "neighbor_updates"],
    );
    let node = DebugNode {
        pos: [1, 2, 3],
        walked_distance: 1.5,
        cost_malus: 0.0,
        closed: true,
        path_type: 1,
        f: 2.5,
    };
    let entity_values = [
        (
            DebugValue::Path(Box::new(DebugPath {
                reached: false,
                next_node: 1,
                target: pos,
                nodes: vec![node, node],
                open_set: vec![node],
                closed_set: vec![],
                targets: vec![node],
                max_node_distance: 0.75,
            })),
            5,
            "max_node_distance: 0.75",
        ),
        (
            DebugValue::Goals(vec![DebugGoal {
                priority: 2,
                running: true,
                name: "FloatGoal".into(),
            }]),
            4,
            "FloatGoal",
        ),
        (
            DebugValue::Brain(Box::new(DebugBrain {
                name: "Villager".into(),
                profession: "none".into(),
                memories: vec!["home: x".into()],
                pois: vec![pos],
                ..Default::default()
            })),
            2,
            "potential_pois: []",
        ),
        (
            DebugValue::Bee {
                hive: Some(pos),
                flower: None,
                travel_ticks: 7,
                blacklisted_hives: vec![pos],
            },
            1,
            "travel_ticks: 7",
        ),
        (
            DebugValue::Breeze {
                attack_target: Some(9),
                jump_target: None,
            },
            3,
            "attack_target: 9",
        ),
        (DebugValue::GameEventListener { radius: 8 }, 13, "radius: 8"),
    ];
    for (value, subscription, needle) in entity_values {
        has(
            &play(&DebugEntityValue {
                entity: 4,
                update: DebugUpdate {
                    subscription,
                    value: Some(value),
                },
            }),
            &["DebugEntityValue", needle],
        );
    }
    let chest_box = DebugBox {
        min: pos,
        max: BlockPos::new(4, 5, 6),
    };
    has(
        &play(&DebugChunkValue {
            x: -1,
            z: 2,
            update: DebugUpdate {
                subscription: 12,
                value: Some(DebugValue::Structures(vec![DebugStructure {
                    bounds: chest_box,
                    pieces: vec![chest_box, chest_box],
                }])),
            },
        }),
        &[
            "DebugChunkValue",
            "structures",
            "start: true",
            "start: false",
        ],
    );
    has(
        &play(&DebugBlockValue {
            pos,
            update: DebugUpdate {
                subscription: 7,
                value: Some(DebugValue::Hive {
                    block: tuff_vanilla_data::registry::Block::Beehive.id() as i32,
                    occupants: 2,
                    honey_level: 5,
                    sedated: true,
                }),
            },
        }),
        &["bee_hives", "minecraft:beehive", "sedated: true"],
    );
    has(
        &play(&DebugBlockValue {
            pos,
            update: DebugUpdate {
                subscription: 6,
                value: Some(DebugValue::BlockIntersection(1)),
            },
        }),
        &["entity_block_intersections", "1"],
    );
    has(
        &play(&DebugBlockValue {
            pos,
            update: DebugUpdate {
                subscription: 13,
                value: Some(DebugValue::GameEventListener { radius: 16 }),
            },
        }),
        &["game_event_listeners", "radius: 16"],
    );
    has(
        &play(&BlockDestruction {
            id: 1,
            position: pos,
            stage: 4,
        }),
        &["stage: 4"],
    );
    has(
        &play(&BlockEvent {
            position: pos,
            action: 1,
            parameter: 2,
            block: tuff_vanilla_data::registry::Block::Chest.id(),
        }),
        &["block: minecraft:chest"],
    );
    let nbt = tuff_nbt::Compound::default();
    has(
        &play(&BlockEntityData {
            position: pos,
            kind: tuff_vanilla_data::registry::BlockEntityType::Sign.id(),
            nbt: &nbt,
        }),
        &["kind: minecraft:sign"],
    );
    has(
        &play(&OpenSignEditor {
            position: pos,
            front: true,
        }),
        &["front: true"],
    );
    has(
        &play(&LevelEvent {
            event: 2001,
            position: pos,
            data: 1,
            global: false,
        }),
        &["event: 2001"],
    );
    has(
        &play(&Sound {
            sound_event: tuff_vanilla_data::registry::SoundEvent::EntityPlayerHurt.id(),
            category: 7,
            position: [1.0, 2.0, 3.5],
            volume: 1.0,
            pitch: 1.0,
            seed: 9,
        }),
        &[
            "sound: minecraft:entity.player.hurt",
            "position: [1.0, 2.0, 3.5]",
        ],
    );
    has(
        &play(&NamedSound {
            sound: "custom:thing",
            category: 0,
            position: [0.0; 3],
            volume: 1.0,
            pitch: 1.0,
            seed: 0,
        }),
        &["sound: Inline { name: \"custom:thing\", range: None }"],
    );
    has(
        &play(&StopSound {
            category: Some(2),
            sound: None,
        }),
        &["category: 2, sound: None"],
    );
    has(
        &play(&Explode {
            center: [0.0; 3],
            radius: 4.0,
            block_count: 3,
            knockback: Some([0.1, 0.2, 0.3]),
            particle: 1,
            sound: 1,
            block_particles: &[(1, 1.0, 1.0, 1)],
        }),
        &["knockback: [0.1, 0.2, 0.3]", "show: true"],
    );
    has(
        &play(&Particles {
            particle: 0,
            position: [0.0; 3],
            spread: [0.5; 3],
            speed: 0.1,
            count: 4,
            alternative: false,
        }),
        &["count: 4"],
    );
    has(&play(&ChunkBatchStart), &["ChunkBatchStart"]);
    has(
        &play(&crate::protocol::play::LowDiskSpaceWarning),
        &["LowDiskSpaceWarning"],
    );
    has(&play(&ChunkBatchFinished(9)), &["size: 9"]);
    has(&play(&ForgetLevelChunk { z: 1, x: 2 }), &["z: 1, x: 2"]);
    has(&play(&SetChunkCacheCenter { x: 1, z: 2 }), &["x: 1, z: 2"]);
    let mut chunk = PacketWriter::new();
    chunk.write_int(4);
    chunk.write_int(-5);
    chunk.write_bytes(&[0; 10]);
    has(
        &play(&LevelChunkWithLight(chunk.into_bytes())),
        &["x: 4, z: -5, data: <10 bytes>"],
    );
    let mut light = PacketWriter::new();
    light.write_var_int(1);
    light.write_var_int(2);
    has(&play(&LightUpdate(light.into_bytes())), &["x: 1, z: 2"]);
    has(
        &play(&InitializeBorder {
            center: [0.0, 0.0],
            old_size: 100.0,
            new_size: 100.0,
            lerp_time: 0,
            absolute_max_size: 29_999_984,
            warning_blocks: 5,
            warning_time: 15,
        }),
        &["new_diameter: 100.0"],
    );
    has(
        &play(&SetDefaultSpawnPosition {
            dimension: "minecraft:overworld",
            location: pos,
            yaw: 0.0,
            pitch: 0.0,
        }),
        &["location: (1, -2, 3)"],
    );
    has(
        &play(&SetTime {
            world_age: 10,
            clocks: vec![ClockTime {
                clock: 0,
                total_ticks: 6000,
                partial_tick: 0.0,
                rate: 1.0,
            }],
        }),
        &["total_ticks: 6000"],
    );
    has(&play(&KeepAlive(7)), &["id: 7"]);
    has(
        &play(&ChangeDifficulty {
            difficulty: 2,
            locked: false,
        }),
        &["difficulty: 2"],
    );
    has(
        &play(&SetTickingState {
            rate: 20.0,
            frozen: true,
        }),
        &["frozen: true"],
    );
    has(&play(&StepTick(3)), &["steps: 3"]);
    let title = TextComponent::new("Boss");
    has(
        &play(&BossEvent {
            uuid: 1,
            action: BossAction::Add {
                title: &title,
                progress: 1.0,
                color: 2,
                overlay: 0,
                flags: 1,
            },
        }),
        &["action: Add { title: \"Boss\", progress: 1.0"],
    );
    has(
        &play(&BossEvent {
            uuid: 1,
            action: BossAction::Progress(0.5),
        }),
        &["action: Progress { progress: 0.5 }"],
    );
    has(
        &play(&BossEvent {
            uuid: 1,
            action: BossAction::Style {
                color: 4,
                overlay: 2,
            },
        }),
        &["action: Style { color: 4, overlay: 2 }"],
    );
    has(
        &play(&BossEvent {
            uuid: 1,
            action: BossAction::Flags(5),
        }),
        &["action: Flags { flags: 5 }"],
    );
    let markers = [MapMarker {
        kind: 0,
        x: 1,
        y: 2,
        rotation: 3,
        name: None,
    }];
    let colors = [0u8; 4];
    has(
        &play(&MapData {
            id: 3,
            scale: 0,
            locked: false,
            markers: Some(&markers),
            patch: Some(MapPatch {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
                colors: &colors,
            }),
        }),
        &["rotation: 3", "colors: <4 bytes>"],
    );
}

#[tuff_core::watchdog]
#[test]
fn chat_and_scoreboard_packets_round_trip() {
    let text = TextComponent::new("hi");
    has(
        &play(&DisguisedChat {
            message: &text,
            chat_type: 0,
            sender: &text,
            target: None,
        }),
        &["chat_type: minecraft:", "target: None"],
    );
    has(
        &play(&CommandSuggestions {
            id: 1,
            start: 2,
            length: 3,
            matches: &["a".into()],
            tooltips: &[Some(text.clone())],
        }),
        &["matches: [{ text: \"a\", tooltip: \"hi\" }]"],
    );
    has(
        &play(&SelectAdvancementsTab(Some("minecraft:story/root"))),
        &["story/root"],
    );
    let icon = ItemStack::mint(Item::GrassBlock, 1, MintReason::Test);
    has(
        &play(&UpdateAdvancements {
            reset: true,
            added: vec![AdvancementEntry {
                id: "minecraft:story/root",
                parent: None,
                display: Some(AdvancementDisplay {
                    title: &text,
                    description: &text,
                    icon: icon.view(),
                    frame: 0,
                    background: Some("minecraft:block/dirt"),
                    show_toast: true,
                    hidden: false,
                }),
                requirements: &[Box::new([tuff_vanilla_data::Frozen::Static("crit")])],
                sends_telemetry_event: false,
                position: [0.0, 1.0],
            }],
            removed: vec![],
            progress: vec![(
                "minecraft:story/root",
                &[(tuff_vanilla_data::Frozen::Static("crit"), Some(5))],
            )],
            show_advancements: true,
        }),
        &[
            "icon: minecraft:grass_block x1",
            "background: \"minecraft:block/dirt\"",
            "time: 5",
        ],
    );
    let format = NumberFormat::Fixed(text.clone());
    has(
        &play(&SetObjective {
            name: "kills",
            method: 0,
            display_name: &text,
            render_type: RenderType::Hearts,
            number_format: Some(&format),
        }),
        &["method: Create { display_name: \"hi\", render_type: 1, number_format: Fixed"],
    );
    has(
        &play(&SetObjective {
            name: "kills",
            method: 1,
            display_name: &text,
            render_type: RenderType::Integer,
            number_format: None,
        }),
        &["method: Remove"],
    );
    has(
        &play(&SetDisplayObjective {
            slot: 1,
            name: "kills",
        }),
        &["slot: 1"],
    );
    has(
        &play(&SetScore {
            holder: "Steve",
            objective: "kills",
            value: 3,
            display: None,
            number_format: None,
        }),
        &["value: 3, display: None, number_format: None"],
    );
    has(
        &play(&ResetScore {
            holder: "Steve",
            objective: None,
        }),
        &["objective: None"],
    );
    let mut team = Team::new("red");
    team.members.insert("Steve".into());
    has(
        &play(&SetPlayerTeam {
            name: "red",
            action: TeamAction::Add(&team),
        }),
        &[
            "action: Add { display_name: \"red\"",
            "members: [\"Steve\"]",
        ],
    );
    has(
        &play(&SetPlayerTeam {
            name: "red",
            action: TeamAction::Leave(&["Steve".into()]),
        }),
        &["action: Leave { members: [\"Steve\"] }"],
    );
}

#[tuff_core::watchdog]
#[test]
fn opaque_tails_close_their_brackets() {
    let nodes = [CommandNodeData {
        flags: 0,
        children: vec![],
        redirect: None,
        name: None,
        parser: None,
        suggestions: None,
    }];
    let text = decoded(
        ConnState::Play,
        &Commands {
            nodes: &nodes,
            root: 0,
        },
    );
    assert_eq!(text, "Commands { tree: 1 nodes ..<3 bytes> }");
}

#[tuff_core::watchdog]
#[test]
fn configuration_packets_round_trip() {
    let c = ConnState::Configuration;
    has(
        &decoded(
            c,
            &cfg::RegistryData {
                registry: "minecraft:dimension_type",
                entries: &["minecraft:overworld"],
                custom: &[],
            },
        ),
        &["entries: [{ id: \"minecraft:overworld\", data: None }]"],
    );
    has(
        &decoded(c, &cfg::FeatureFlags(&["minecraft:vanilla"])),
        &["features: [\"minecraft:vanilla\"]"],
    );
    has(
        &decoded(
            c,
            &cfg::KnownPacks(&[cfg::KnownPack {
                namespace: "minecraft".into(),
                id: "core".into(),
                version: "1".into(),
            }]),
        ),
        &["namespace: \"minecraft\""],
    );
    has(
        &decoded(c, &cfg::FinishConfiguration),
        &["FinishConfiguration"],
    );
    has(
        &decoded(
            c,
            &cfg::PluginMessage {
                channel: "minecraft:brand",
                data: b"tuff",
            },
        ),
        &["channel: \"minecraft:brand\", data: <4 bytes>"],
    );
    let tags = decoded(c, &cfg::update_tags());
    assert!(
        tags.starts_with("UpdateTags { registries: [{ registry: \"minecraft:block\""),
        "{}",
        &tags[..200]
    );
    let text = TextComponent::new("bye");
    has(
        &decoded(c, &pack::ConfigDisconnect(&text)),
        &["reason: \"bye\""],
    );
    let push = |prompt| pack::Push {
        id: 1,
        url: "http://x",
        hash: "",
        required: true,
        prompt,
    };
    has(
        &decoded(c, &pack::ConfigPush(push(Some(&text)))),
        &["prompt: \"bye\""],
    );
    has(
        &play(&pack::PlayPush(push(None))),
        &["required: true, prompt: None"],
    );
    has(&decoded(c, &pack::ConfigPop(Some(1))), &["id: 0000"]);
    has(&play(&pack::PlayPop(None)), &["id: None"]);
}

#[tuff_core::watchdog]
#[test]
fn login_finished_decodes_through_the_schema() {
    let profile = crate::auth::GameProfile {
        id: 5,
        name: "Alex".into(),
        properties: vec![],
    };
    has(
        &decoded(
            ConnState::Login,
            &cfg::LoginSuccess {
                profile: &profile,
                session_id: 6,
            },
        ),
        &["LoginFinished { uuid: ", "name: \"Alex\", properties: []"],
    );
}

#[tuff_core::watchdog]
#[test]
fn hurt_animation_tab_list_tag_query_and_bundle_decode() {
    has(
        &play(&HurtAnimation {
            entity_id: 4,
            yaw: 90.0,
        }),
        &["HurtAnimation", "90"],
    );
    let header = tuff_item::text::TextComponent::new("top");
    let footer = tuff_item::text::TextComponent::new("bottom");
    has(
        &play(&TabList {
            header: &header,
            footer: &footer,
        }),
        &["TabList", "top", "bottom"],
    );
    has(
        &play(&TagQuery {
            transaction_id: 3,
            tag: None,
        }),
        &["TagQuery"],
    );
    has(&play(&BundleDelimiter), &["BundleDelimiter"]);
}
