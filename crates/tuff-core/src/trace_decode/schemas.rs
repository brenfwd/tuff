use super::*;

pub(super) const OPAQUE: F = F::Custom(opaque);
pub(super) const TEXT_OPT: F = F::Opt(&F::Text);
pub(super) const NUMBER_FORMAT: F = F::Opt(&F::VarTag(&[
    ("Blank", &[]),
    ("Styled", &[("style", F::Nbt)]),
    ("Fixed", &[("text", F::Text)]),
]));
pub(super) const PROFILE_PROPERTY: F = F::Obj(&[
    ("name", F::Str),
    ("value", F::Str),
    ("signature", F::Opt(&F::Str)),
]);
pub(super) const SOUND: F = F::Holder(
    sound_event,
    &[("name", F::Str), ("range", F::Opt(&F::Float))],
);
pub(super) const SPAWN_INFO: Fields = &[
    ("dimension_type", F::Reg(dimension_type)),
    ("dimension", F::Str),
    ("hashed_seed", F::Long),
    ("game_mode", F::Reg(game_mode)),
    ("previous_game_mode", F::RegPlusOne(game_mode)),
    ("debug", F::Bool),
    ("flat", F::Bool),
    (
        "death_location",
        F::Opt(&F::Obj(&[("dimension", F::Str), ("pos", F::Pos)])),
    ),
    ("portal_cooldown", F::VarInt),
    ("sea_level", F::VarInt),
];
pub(super) const TEAM_PARAMS: Fields = &[
    ("display_name", F::Text),
    ("prefix", F::Text),
    ("suffix", F::Text),
    ("name_tag_visibility", F::VarInt),
    ("collision_rule", F::VarInt),
    ("color", F::Opt(&F::VarInt)),
    ("options", F::Byte),
];
pub(super) const OBJECTIVE: Fields = &[
    ("display_name", F::Text),
    ("render_type", F::VarInt),
    ("number_format", NUMBER_FORMAT),
];
pub(super) const RESOURCE_PACK_PUSH: Fields = &[
    ("id", F::Uuid),
    ("url", F::Str),
    ("hash", F::Str),
    ("required", F::Bool),
    ("prompt", TEXT_OPT),
];

macro_rules! concat_fields {
    ($a:expr, $b:expr) => {{
        const A: Fields = $a;
        const B: Fields = $b;
        const N: usize = A.len() + B.len();
        const OUT: [(&str, F); N] = {
            let mut out = [("", F::Bool); N];
            let mut i = 0;
            while i < A.len() {
                out[i] = A[i];
                i += 1;
            }
            while i < N {
                out[i] = B[i - A.len()];
                i += 1;
            }
            out
        };
        &OUT
    }};
}

pub(super) const LOGIN_HEAD: Fields = &[
    ("entity_id", F::Int),
    ("hardcore", F::Bool),
    ("dimensions", F::List(&F::Str)),
    ("max_players", F::VarInt),
    ("view_distance", F::VarInt),
    ("simulation_distance", F::VarInt),
    ("reduced_debug_info", F::Bool),
    ("show_death_screen", F::Bool),
    ("limited_crafting", F::Bool),
];
pub(super) const LOGIN_TAIL: Fields =
    &[("online_mode", F::Bool), ("enforces_secure_chat", F::Bool)];
pub(super) const LOGIN: Fields = concat_fields!(LOGIN_HEAD, concat_fields!(SPAWN_INFO, LOGIN_TAIL));
pub(super) const RESPAWN: Fields = concat_fields!(SPAWN_INFO, &[("data_kept", F::Byte)]);
pub(super) const TEAM_ADD: Fields = concat_fields!(TEAM_PARAMS, &[("members", F::List(&F::Str))]);

pub(super) const UPDATE_TAGS: Fields = &[(
    "registries",
    F::List(&F::Obj(&[
        ("registry", F::Str),
        (
            "tags",
            F::List(&F::Obj(&[
                ("name", F::Str),
                ("entries", F::List(&F::VarInt)),
            ])),
        ),
    ])),
)];

pub(super) const LOGIN_FINISHED: Fields = &[
    ("uuid", F::Uuid),
    ("name", F::Str),
    ("properties", F::List(&PROFILE_PROPERTY)),
    ("session_id", F::Uuid),
];

pub(super) const CONFIGURATION: &[(p::configuration::Clientbound, Fields)] = {
    use p::configuration::Clientbound as C;
    &[
        (C::CustomPayload, &[("channel", F::Str), ("data", F::Rest)]),
        (C::Disconnect, &[("reason", F::Text)]),
        (C::FinishConfiguration, &[]),
        (C::KeepAlive, &[("id", F::Long)]),
        (C::Ping, &[("id", F::Int)]),
        (
            C::RegistryData,
            &[
                ("registry", F::Str),
                (
                    "entries",
                    F::List(&F::Obj(&[("id", F::Str), ("data", F::Opt(&F::Nbt))])),
                ),
            ],
        ),
        (C::ResourcePackPop, &[("id", F::Opt(&F::Uuid))]),
        (C::ResourcePackPush, RESOURCE_PACK_PUSH),
        (C::CodeOfConduct, &[("text", F::Str)]),
        (C::ClearDialog, &[]),
        (
            C::CustomReportDetails,
            &[(
                "details",
                F::List(&F::Obj(&[("key", F::Str), ("value", F::Str)])),
            )],
        ),
        (C::ShowDialog, &[("dialog", F::Nbt)]),
        (
            C::ServerLinks,
            &[(
                "links",
                F::List(&F::Obj(&[
                    ("known", F::Bool),
                    ("type", F::VarInt),
                    ("url", F::Str),
                ])),
            )],
        ),
        (C::UpdateEnabledFeatures, &[("features", F::List(&F::Str))]),
        (C::UpdateTags, UPDATE_TAGS),
        (
            C::SelectKnownPacks,
            &[(
                "packs",
                F::List(&F::Obj(&[
                    ("namespace", F::Str),
                    ("id", F::Str),
                    ("version", F::Str),
                ])),
            )],
        ),
    ]
};

/// Never sent by the server.
#[cfg_attr(not(test), expect(dead_code, reason = "read only by the schema tests"))]
pub(super) const CONFIGURATION_SKIPPED: &[p::configuration::Clientbound] = {
    use p::configuration::Clientbound as C;
    &[
        C::CookieRequest,
        C::ResetChat,
        C::PostEffects,
        C::StoreCookie,
        C::Transfer,
    ]
};

pub(super) const PLAY: &[(p::play::Clientbound, Fields)] = {
    use p::play::Clientbound as C;
    &[
        (C::CustomPayload, &[("channel", F::Str), ("data", F::Rest)]),
        (
            C::AddEntity,
            &[
                ("entity_id", F::VarInt),
                ("uuid", F::Uuid),
                ("kind", F::Reg(entity_type)),
                ("position", F::Vec3),
                ("velocity", F::LpVec3),
                ("pitch", F::Angle),
                ("yaw", F::Angle),
                ("head_yaw", F::Angle),
                ("data", F::VarInt),
            ],
        ),
        (
            C::Animate,
            &[("entity_id", F::VarInt), ("animation", F::UByte)],
        ),
        (
            C::AwardStats,
            &[("stats", F::List(&F::Fixed(3, &F::VarInt)))],
        ),
        (C::BlockChangedAck, &[("sequence", F::VarInt)]),
        (
            C::BlockDestruction,
            &[("id", F::VarInt), ("position", F::Pos), ("stage", F::Byte)],
        ),
        (
            C::BlockEntityData,
            &[
                ("position", F::Pos),
                ("kind", F::Reg(block_entity)),
                ("nbt", F::Nbt),
            ],
        ),
        (
            C::BlockEvent,
            &[
                ("position", F::Pos),
                ("action", F::UByte),
                ("parameter", F::UByte),
                ("block", F::Reg(block)),
            ],
        ),
        (
            C::BlockUpdate,
            &[("position", F::Pos), ("state", F::Reg(block_state))],
        ),
        (
            C::BossEvent,
            &[
                ("uuid", F::Uuid),
                (
                    "action",
                    F::VarTag(&[
                        (
                            "Add",
                            &[
                                ("title", F::Text),
                                ("progress", F::Float),
                                ("color", F::VarInt),
                                ("overlay", F::VarInt),
                                ("flags", F::UByte),
                            ],
                        ),
                        ("Remove", &[]),
                        ("Progress", &[("progress", F::Float)]),
                        ("Title", &[("title", F::Text)]),
                        ("Style", &[("color", F::VarInt), ("overlay", F::VarInt)]),
                        ("Flags", &[("flags", F::UByte)]),
                    ]),
                ),
            ],
        ),
        (
            C::ChangeDifficulty,
            &[("difficulty", F::UByte), ("locked", F::Bool)],
        ),
        (C::ChunkBatchFinished, &[("size", F::VarInt)]),
        (C::ChunkBatchStart, &[]),
        (C::LowDiskSpaceWarning, &[]),
        (C::ClearTitles, &[("reset", F::Bool)]),
        (
            C::CommandSuggestions,
            &[
                ("id", F::VarInt),
                ("start", F::VarInt),
                ("length", F::VarInt),
                (
                    "matches",
                    F::List(&F::Obj(&[("text", F::Str), ("tooltip", TEXT_OPT)])),
                ),
            ],
        ),
        (C::Commands, &[("tree", F::Custom(commands))]),
        (C::ContainerClose, &[("window_id", F::VarInt)]),
        (
            C::ContainerSetContent,
            &[
                ("window_id", F::VarInt),
                ("state_id", F::VarInt),
                ("slots", F::List(&F::Slot)),
                ("carried", F::Slot),
            ],
        ),
        (
            C::ContainerSetData,
            &[
                ("window_id", F::VarInt),
                ("property", F::Short),
                ("value", F::Short),
            ],
        ),
        (
            C::ContainerSetSlot,
            &[
                ("window_id", F::VarInt),
                ("state_id", F::VarInt),
                ("slot", F::Short),
                ("item", F::Slot),
            ],
        ),
        (C::Cooldown, &[("group", F::Str), ("ticks", F::VarInt)]),
        (
            C::DamageEvent,
            &[
                ("entity_id", F::VarInt),
                ("damage_type", F::Reg(damage_type)),
                ("cause", F::RegPlusOne(no_name)),
                ("direct", F::RegPlusOne(no_name)),
                ("source_position", F::Opt(&F::Vec3)),
            ],
        ),
        (C::Disconnect, &[("reason", F::Text)]),
        (
            C::DisguisedChat,
            &[
                ("message", F::Text),
                ("chat_type", F::RegPlusOne(chat_type)),
                ("sender", F::Text),
                ("target", TEXT_OPT),
            ],
        ),
        (
            C::EntityEvent,
            &[("entity_id", F::Int), ("status", F::Byte)],
        ),
        (
            C::EntityPositionSync,
            &[
                ("entity_id", F::VarInt),
                ("path_kind", F::VarInt),
                ("position", F::Vec3),
                ("yaw", F::Float),
                ("pitch", F::Float),
                ("on_ground", F::Bool),
            ],
        ),
        (
            C::Explode,
            &[
                ("center", F::Vec3),
                ("radius", F::Float),
                ("block_count", F::Int),
                ("knockback", F::Opt(&F::Vec3)),
                ("particle", F::Reg(particle)),
                ("sound", SOUND),
                (
                    "block_particles",
                    F::List(&F::Obj(&[
                        ("particle", F::Reg(particle)),
                        ("scaling", F::Float),
                        ("speed", F::Float),
                        ("weight", F::VarInt),
                    ])),
                ),
                ("show", F::Bool),
            ],
        ),
        (
            C::DebugBlockValue,
            &[("pos", F::Pos), ("update", DEBUG_UPDATE)],
        ),
        (
            C::DebugChunkValue,
            &[("chunk", F::Long), ("update", DEBUG_UPDATE)],
        ),
        (
            C::DebugEntityValue,
            &[("entity", F::VarInt), ("update", DEBUG_UPDATE)],
        ),
        (C::DebugEvent, &[("event", F::VarTag(DEBUG_EVENTS))]),
        (
            C::DebugSample,
            &[("sample", F::List(&F::Long)), ("type", F::VarInt)],
        ),
        (C::ForgetLevelChunk, &[("z", F::Int), ("x", F::Int)]),
        (
            C::GameEvent,
            &[("event", F::Custom(game_event)), ("value", F::Float)],
        ),
        (
            C::InitializeBorder,
            &[
                ("center", F::Fixed(2, &F::Double)),
                ("old_diameter", F::Double),
                ("new_diameter", F::Double),
                ("lerp_ticks", F::VarLong),
                ("portal_bound", F::VarInt),
                ("warning_blocks", F::VarInt),
                ("warning_time", F::VarInt),
            ],
        ),
        (
            C::ChunksBiomes,
            &[
                ("count", F::VarInt),
                ("z", F::Int),
                ("x", F::Int),
                ("data", F::Rest),
            ],
        ),
        (C::KeepAlive, &[("id", F::Long)]),
        (
            C::LevelChunkWithLight,
            &[("x", F::Int), ("z", F::Int), ("data", F::Rest)],
        ),
        (
            C::LevelEvent,
            &[
                ("event", F::Int),
                ("position", F::Pos),
                ("data", F::Int),
                ("global", F::Bool),
            ],
        ),
        (
            C::LevelParticles,
            &[
                ("particle", F::Custom(particle_with_options)),
                ("long_distance", F::Bool),
                ("always_show", F::Bool),
                ("position", F::Vec3),
                ("spread", F::Fixed(3, &F::Float)),
                ("speed", F::Fixed(3, &F::Float)),
                ("count", F::VarInt),
                ("randomization", F::VarInt),
            ],
        ),
        (
            C::LightUpdate,
            &[("x", F::VarInt), ("z", F::VarInt), ("data", F::Rest)],
        ),
        (C::Login, LOGIN),
        (
            C::MapItemData,
            &[
                ("id", F::VarInt),
                ("scale", F::Byte),
                ("locked", F::Bool),
                (
                    "markers",
                    F::Opt(&F::List(&F::Obj(&[
                        ("kind", F::Reg(map_decoration)),
                        ("x", F::Byte),
                        ("y", F::Byte),
                        ("rotation", F::Byte),
                        ("name", TEXT_OPT),
                    ]))),
                ),
                ("patch", F::Custom(map_patch)),
            ],
        ),
        (
            C::MerchantOffers,
            &[
                ("window_id", F::VarInt),
                (
                    "offers",
                    F::List(&F::Obj(&[
                        ("buy", F::Custom(trade_cost)),
                        ("sell", F::Slot),
                        ("buy_b", F::Opt(&F::Custom(trade_cost))),
                        ("out_of_stock", F::Bool),
                        ("uses", F::Int),
                        ("max_uses", F::Int),
                        ("xp", F::Int),
                        ("special_price", F::Int),
                        ("price_multiplier", F::Float),
                        ("demand", F::Int),
                    ])),
                ),
                ("level", F::VarInt),
                ("xp", F::VarInt),
                ("regular", F::Bool),
                ("can_restock", F::Bool),
            ],
        ),
        (
            C::MoveVehicle,
            &[
                ("position", F::Vec3),
                ("yaw", F::Float),
                ("pitch", F::Float),
            ],
        ),
        (
            C::OpenScreen,
            &[
                ("window_id", F::VarInt),
                ("menu", F::Reg(menu)),
                ("title", F::Text),
            ],
        ),
        (
            C::OpenSignEditor,
            &[("position", F::Pos), ("front", F::Bool)],
        ),
        (C::OpenBook, &[("hand", F::VarInt)]),
        (C::ClearDialog, &[]),
        (C::ShowDialog, &[("holder", F::VarInt), ("dialog", F::Nbt)]),
        (
            C::PlaceGhostRecipe,
            &[("window_id", F::VarInt), ("display", OPAQUE)],
        ),
        (
            C::PlayerAbilities,
            &[
                ("flags", F::Byte),
                ("flying_speed", F::Float),
                ("fov_modifier", F::Float),
            ],
        ),
        (
            C::PlayerCombatKill,
            &[("player_id", F::VarInt), ("message", F::Text)],
        ),
        (C::PlayerInfoRemove, &[("uuids", F::List(&F::Uuid))]),
        (
            C::PlayerInfoUpdate,
            &[("actions", F::Custom(player_info_update))],
        ),
        (
            C::PlayerPosition,
            &[
                ("teleport_id", F::VarInt),
                ("position", F::Vec3),
                ("delta", F::Vec3),
                ("yaw", F::Float),
                ("pitch", F::Float),
                ("relative", F::Int),
            ],
        ),
        (C::RecipeBookAdd, &[("entries", F::Custom(recipe_book_add))]),
        (C::RecipeBookRemove, &[("ids", F::List(&F::VarInt))]),
        (
            C::RecipeBookSettings,
            &[("books", F::Fixed(4, &F::Fixed(2, &F::Bool)))],
        ),
        (C::RemoveEntities, &[("ids", F::List(&F::VarInt))]),
        (
            C::RemoveMobEffect,
            &[("entity_id", F::VarInt), ("effect", F::Reg(mob_effect))],
        ),
        (
            C::ResetScore,
            &[("holder", F::Str), ("objective", F::Opt(&F::Str))],
        ),
        (C::ResourcePackPop, &[("id", F::Opt(&F::Uuid))]),
        (C::ResourcePackPush, RESOURCE_PACK_PUSH),
        (C::Respawn, RESPAWN),
        (
            C::MoveEntityPos,
            &[
                ("entity_id", F::VarInt),
                ("properties", F::VarInt),
                ("delta", F::Fixed(3, &F::Short)),
            ],
        ),
        (
            C::MoveEntityPosRot,
            &[
                ("entity_id", F::VarInt),
                ("properties", F::VarInt),
                ("delta", F::Fixed(3, &F::Short)),
                ("yaw", F::Angle),
                ("pitch", F::Angle),
            ],
        ),
        (
            C::MoveEntityRot,
            &[
                ("entity_id", F::VarInt),
                ("on_ground", F::Bool),
                ("yaw", F::Angle),
                ("pitch", F::Angle),
            ],
        ),
        (
            C::MoveMinecartAlongTrack,
            &[
                ("entity_id", F::VarInt),
                (
                    "steps",
                    F::List(&F::Obj(&[
                        ("position", F::Vec3),
                        ("velocity", F::Vec3),
                        ("yaw", F::Angle),
                        ("pitch", F::Angle),
                        ("weight", F::Float),
                    ])),
                ),
            ],
        ),
        (
            C::RotateHead,
            &[("entity_id", F::VarInt), ("head_yaw", F::Angle)],
        ),
        (
            C::SectionBlocksUpdate,
            &[("section", F::Custom(section_blocks))],
        ),
        (C::SelectAdvancementsTab, &[("tab", F::Opt(&F::Str))]),
        (C::SetActionBarText, &[("text", F::Text)]),
        (
            C::SetChunkCacheCenter,
            &[("x", F::VarInt), ("z", F::VarInt)],
        ),
        (C::SetCursorItem, &[("item", F::Slot)]),
        (
            C::SetDefaultSpawnPosition,
            &[
                ("dimension", F::Str),
                ("location", F::Pos),
                ("yaw", F::Float),
                ("pitch", F::Float),
            ],
        ),
        (
            C::SetDisplayObjective,
            &[("slot", F::VarInt), ("name", F::Str)],
        ),
        (
            C::SetEntityData,
            &[
                ("entity_id", F::VarInt),
                ("values", F::Custom(entity_metadata)),
            ],
        ),
        (
            C::SetEntityMotion,
            &[("entity_id", F::VarInt), ("velocity", F::LpVec3)],
        ),
        (
            C::SetEquipment,
            &[("entity_id", F::VarInt), ("items", F::Custom(equipment))],
        ),
        (
            C::SetExperience,
            &[
                ("progress", F::Float),
                ("level", F::VarInt),
                ("total", F::VarInt),
            ],
        ),
        (
            C::SetHealth,
            &[
                ("health", F::Float),
                ("food", F::VarInt),
                ("saturation", F::Float),
            ],
        ),
        (C::SetHeldSlot, &[("slot", F::VarInt)]),
        (
            C::SetObjective,
            &[
                ("name", F::Str),
                (
                    "method",
                    F::ByteTag(&[
                        ("Create", OBJECTIVE),
                        ("Remove", &[]),
                        ("Update", OBJECTIVE),
                    ]),
                ),
            ],
        ),
        (
            C::SetPassengers,
            &[("vehicle", F::VarInt), ("passengers", F::List(&F::VarInt))],
        ),
        (
            C::SetPlayerTeam,
            &[
                ("name", F::Str),
                (
                    "action",
                    F::ByteTag(&[
                        ("Add", TEAM_ADD),
                        ("Remove", &[]),
                        ("Change", TEAM_PARAMS),
                        ("Join", &[("members", F::List(&F::Str))]),
                        ("Leave", &[("members", F::List(&F::Str))]),
                    ]),
                ),
            ],
        ),
        (
            C::SetScore,
            &[
                ("holder", F::Str),
                ("objective", F::Str),
                ("value", F::VarInt),
                ("display", TEXT_OPT),
                ("number_format", NUMBER_FORMAT),
            ],
        ),
        (
            C::GameRuleValues,
            &[(
                "rules",
                F::List(&F::Obj(&[("rule", F::Str), ("value", F::Str)])),
            )],
        ),
        (
            C::PlayerLookAt,
            &[
                ("from", F::VarInt),
                ("position", F::Fixed(3, &F::Double)),
                (
                    "entity",
                    F::Opt(&F::Obj(&[("entity_id", F::VarInt), ("to", F::VarInt)])),
                ),
            ],
        ),
        (C::SetSubtitleText, &[("text", F::Text)]),
        (
            C::SetTime,
            &[
                ("world_age", F::Long),
                (
                    "clocks",
                    F::List(&F::Obj(&[
                        ("clock", F::VarInt),
                        ("total_ticks", F::VarLong),
                        ("partial_tick", F::Float),
                        ("rate", F::Float),
                    ])),
                ),
            ],
        ),
        (C::SetTitleText, &[("text", F::Text)]),
        (
            C::SetTitlesAnimation,
            &[("fade_in", F::Int), ("stay", F::Int), ("fade_out", F::Int)],
        ),
        (
            C::Sound,
            &[
                ("sound", SOUND),
                ("category", F::VarInt),
                ("position", F::Custom(sound_position)),
                ("volume", F::Float),
                ("pitch", F::Float),
                ("seed", F::Long),
            ],
        ),
        (C::StopSound, &[("filter", F::Custom(stop_sound))]),
        (
            C::SwingAnimation,
            &[
                ("entity_id", F::VarInt),
                ("hand", F::VarInt),
                ("kind", F::VarInt),
                ("duration", F::VarInt),
            ],
        ),
        (C::SystemChat, &[("content", F::Text), ("overlay", F::Bool)]),
        (
            C::TakeItemEntity,
            &[
                ("collected", F::VarInt),
                ("collector", F::VarInt),
                ("count", F::VarInt),
            ],
        ),
        (C::TickingState, &[("rate", F::Float), ("frozen", F::Bool)]),
        (C::TickingStep, &[("steps", F::VarInt)]),
        (
            C::UpdateAdvancements,
            &[
                ("reset", F::Bool),
                (
                    "added",
                    F::List(&F::Obj(&[
                        ("id", F::Str),
                        ("parent", F::Opt(&F::Str)),
                        (
                            "display",
                            F::Opt(&F::Obj(&[
                                ("title", F::Text),
                                ("description", F::Text),
                                ("icon", F::Template),
                                ("frame", F::VarInt),
                                ("flags", F::Custom(advancement_flags)),
                            ])),
                        ),
                        ("requirements", F::List(&F::List(&F::Str))),
                        ("telemetry", F::Bool),
                        ("position", F::Fixed(2, &F::Float)),
                    ])),
                ),
                ("removed", F::List(&F::Str)),
                (
                    "progress",
                    F::List(&F::Obj(&[
                        ("id", F::Str),
                        (
                            "criteria",
                            F::List(&F::Obj(&[("name", F::Str), ("time", F::Opt(&F::Long))])),
                        ),
                    ])),
                ),
                ("show", F::Bool),
            ],
        ),
        (
            C::UpdateAttributes,
            &[
                ("entity_id", F::VarInt),
                (
                    "properties",
                    F::List(&F::Obj(&[
                        ("attribute", F::Reg(attribute)),
                        ("base", F::Double),
                        (
                            "modifiers",
                            F::List(&F::Obj(&[
                                ("id", F::Str),
                                ("amount", F::Double),
                                ("operation", F::UByte),
                            ])),
                        ),
                    ])),
                ),
            ],
        ),
        (
            C::UpdateMobEffect,
            &[
                ("entity_id", F::VarInt),
                ("effect", F::Reg(mob_effect)),
                ("amplifier", F::VarInt),
                ("duration", F::VarInt),
                ("flags", F::UByte),
            ],
        ),
        (
            C::UpdateRecipes,
            &[
                (
                    "property_sets",
                    F::List(&F::Obj(&[
                        ("name", F::Str),
                        ("items", F::List(&F::Reg(item_name))),
                    ])),
                ),
                ("stonecutter", OPAQUE),
            ],
        ),
        (C::UpdateTags, UPDATE_TAGS),
        (C::Ping, &[("id", F::Int)]),
        (C::PongResponse, &[("time", F::Long)]),
        (C::SetCamera, &[("entity_id", F::VarInt)]),
        (
            C::ServerData,
            &[("motd", F::Text), ("icon", F::Opt(&F::Rest))],
        ),
        (C::SetChunkCacheRadius, &[("radius", F::VarInt)]),
        (C::SetSimulationDistance, &[("distance", F::VarInt)]),
        (C::StartConfiguration, &[]),
        (C::BundleDelimiter, &[]),
        (
            C::HurtAnimation,
            &[("entity_id", F::VarInt), ("yaw", F::Float)],
        ),
        (
            C::TeleportEntity,
            &[
                ("entity_id", F::VarInt),
                ("position", F::Vec3),
                ("delta", F::Vec3),
                ("yaw", F::Float),
                ("pitch", F::Float),
                ("relative", F::Int),
                ("on_ground", F::Bool),
            ],
        ),
        (
            C::SetEntityLink,
            &[("attached", F::Int), ("holder", F::Int)],
        ),
        (
            C::PlayerRotation,
            &[
                ("yaw", F::Float),
                ("relative_yaw", F::Bool),
                ("pitch", F::Float),
                ("relative_pitch", F::Bool),
            ],
        ),
        (C::SetBorderCenter, &[("x", F::Double), ("z", F::Double)]),
        (C::SetBorderSize, &[("diameter", F::Double)]),
        (
            C::SetBorderLerpSize,
            &[
                ("old_diameter", F::Double),
                ("new_diameter", F::Double),
                ("lerp_ticks", F::VarLong),
            ],
        ),
        (C::SetBorderWarningDelay, &[("time", F::VarInt)]),
        (C::SetBorderWarningDistance, &[("blocks", F::VarInt)]),
        (C::PlayerCombatEnter, &[]),
        (C::PlayerCombatEnd, &[("duration", F::VarInt)]),
        (
            C::SetPlayerInventory,
            &[("slot", F::VarInt), ("item", F::Slot)],
        ),
        (
            C::TagQuery,
            &[("transaction_id", F::VarInt), ("tag", F::Nbt)],
        ),
        (C::TabList, &[("header", F::Text), ("footer", F::Text)]),
        (
            C::CustomReportDetails,
            &[(
                "details",
                F::List(&F::Obj(&[("key", F::Str), ("value", F::Str)])),
            )],
        ),
        (C::PostEffects, &[("effects", F::List(&F::Str))]),
        (
            C::CustomChatCompletions,
            &[("action", F::VarInt), ("entries", F::List(&F::Str))],
        ),
        (C::Transfer, &[("host", F::Str), ("port", F::VarInt)]),
        (
            C::MountScreenOpen,
            &[
                ("window_id", F::VarInt),
                ("columns", F::VarInt),
                ("entity_id", F::Int),
            ],
        ),
        (
            C::ProjectilePower,
            &[("entity_id", F::VarInt), ("power", F::Double)],
        ),
        (
            C::SoundEntity,
            &[
                ("sound", SOUND),
                ("category", F::VarInt),
                ("entity_id", F::VarInt),
                ("volume", F::Float),
                ("pitch", F::Float),
                ("seed", F::Long),
            ],
        ),
    ]
};

/// Never sent by the server.
#[cfg_attr(not(test), expect(dead_code, reason = "read only by the schema tests"))]
pub(super) const PLAY_SKIPPED: &[p::play::Clientbound] = {
    use p::play::Clientbound as C;
    &[
        C::CookieRequest,
        C::DeleteChat,
        C::AddTransientBlock,
        C::GameTestHighlightPos,
        C::PlayerChat,
        C::StoreCookie,
        C::TestInstanceBlockStatus,
        C::ServerLinks,
        C::Waypoint,
    ]
};
