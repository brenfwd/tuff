//! Opt-in, non-vanilla: pistons push and pull blocks with block entities
//! (chests, furnaces, hoppers, signs, ...) together with their data. Beds
//! stay immovable and blocks that pop when pushed still break. Moves
//! across region borders hand the data to the destination's region.
//!
//! The moved data rides in the moving piston's saved data under a key
//! clients never see; a save taken mid-move lands every carried block in
//! the saved copy, so vanilla reads the world without losing anything.
//!
//! Clients draw a moving block from its model alone, so blocks drawn by
//! their block entity are shown moving as a block display of their state
//! (standing banners with patterns as an item display of the banner).

use std::sync::Arc;

use tuff_vanilla_data::Block;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::BlockEntityType;

use tuff_core::feature::{Feature, LoadContext};
use tuff_core::storage::chunk_nbt::decode_block_state;
use tuff_core::storage::normalise::{SaveNormaliser, SaveNormaliserEntry, SaveView};
use tuff_core::world::block_entity::raw;
use tuff_core::world::redstone::{BlockEntityCarrier, CarriedLook};
use tuff_item::item::{Components, ItemStack, MintReason};
use tuff_item::load::{LoadError, Phase};
use tuff_nbt::Compound;

pub const ID: &str = "moving-block-entities";

pub struct MovingBlockEntities;

impl Feature for MovingBlockEntities {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            let registries = cx.registries();
            registries
                .block_entity_carrier
                .fill(ID, Arc::new(Carrier))?;
            registries.save_normalisers.push(SaveNormaliserEntry {
                order: 0,
                hook: Arc::new(LandOnSave),
            });
        }
        Ok(())
    }
}

struct Carrier;

impl BlockEntityCarrier for Carrier {
    fn carries(&self, state: BlockState) -> bool {
        let block = state.block();
        block
            .block_entity()
            .is_some_and(|kind| kind != BlockEntityType::Piston && kind.name() != "minecraft:bed")
    }

    fn look(&self, state: BlockState, nbt: Option<&Compound>) -> CarriedLook {
        match state.block().block_entity() {
            Some(BlockEntityType::Banner) => nbt
                .and_then(|nbt| patterned_banner(state, nbt))
                .unwrap_or(CarriedLook::BlockDisplay),
            Some(BlockEntityType::Skull) => nbt
                .and_then(|nbt| profiled_head(state, nbt))
                .unwrap_or(CarriedLook::BlockDisplay),
            Some(kind) if matches!(kind.name(), "minecraft:sign" | "minecraft:hanging_sign") => {
                sign_board(state).unwrap_or(CarriedLook::BlockDisplay)
            }
            Some(kind) if drawn_by_block_entity(kind) => CarriedLook::BlockDisplay,
            _ => CarriedLook::Model,
        }
    }
}

/// Block entities whose blocks clients draw only through the block entity,
/// in a shape a block display reproduces from the block state.
fn drawn_by_block_entity(kind: BlockEntityType) -> bool {
    use BlockEntityType as K;
    matches!(
        kind,
        K::Chest
            | K::TrappedChest
            | K::EnderChest
            | K::ShulkerBox
            | K::Banner
            | K::Skull
            | K::Bell
            | K::Conduit
            | K::DecoratedPot
            | K::EnchantingTable
            | K::EndGateway
            | K::EndPortal
            | K::CopperGolemStatue
    )
}

/// A standing banner with patterns as its item, which alone carries them,
/// turned to stand as the block does.
fn patterned_banner(state: BlockState, nbt: &Compound) -> Option<CarriedLook> {
    let rotation: u8 = state.get("rotation")?.parse().ok()?;
    let components = Components::from_nbt(&raw::collect_components(BlockEntityType::Banner, nbt));
    let patterns = components
        .into_patch()
        .banner_patterns
        .filter(|layers| !layers.is_empty())?;
    let mut item = ItemStack::mint(state.block().item()?, 1, MintReason::Template);
    item.components.banner_patterns = Some(patterns);
    let half = -(f32::from(rotation) * 22.5 + 180.0).to_radians() / 2.0;
    Some(CarriedLook::ItemDisplay {
        item: Box::new(item),
        translation: [0.5; 3],
        rotation: [0.0, half.sin(), 0.0, half.cos()],
    })
}

/// A sign or hanging sign as its item without text, turned and placed like
/// the board.
fn sign_board(state: BlockState) -> Option<CarriedLook> {
    let (angle, offset) = if let Some(rotation) = state.get("rotation") {
        let rotation: f32 = rotation.parse().ok()?;
        (-rotation * 22.5, [0.0; 2])
    } else {
        let (angle, dx, dz) = match state.get("facing")? {
            "south" => (0.0, 0.0, 1.0),
            "west" => (-90.0, -1.0, 0.0),
            "north" => (180.0, 0.0, -1.0),
            "east" => (90.0, 1.0, 0.0),
            _ => return None,
        };
        (angle, [-0.4375 * dx, -0.4375 * dz])
    };
    let half = f32::to_radians(angle) / 2.0;
    Some(CarriedLook::ItemDisplay {
        item: Box::new(ItemStack::mint(
            state.block().item()?,
            1,
            MintReason::Template,
        )),
        translation: [0.5 + offset[0], 0.5, 0.5 + offset[1]],
        rotation: [0.0, half.sin(), 0.0, half.cos()],
    })
}

fn standing_turn(state: BlockState) -> Option<[f32; 4]> {
    let rotation: u8 = state.get("rotation")?.parse().ok()?;
    let half = -(f32::from(rotation) * 22.5 + 180.0).to_radians() / 2.0;
    Some([0.0, half.sin(), 0.0, half.cos()])
}

/// A standing head with a skin profile as its item, which alone carries the
/// skin.
fn profiled_head(state: BlockState, nbt: &Compound) -> Option<CarriedLook> {
    let rotation = standing_turn(state)?;
    let components = Components::from_nbt(&raw::collect_components(BlockEntityType::Skull, nbt));
    components.other.get("minecraft:profile")?;
    let mut item = ItemStack::mint(state.block().item()?, 1, MintReason::Template);
    item.components = components;
    Some(CarriedLook::ItemDisplay {
        item: Box::new(item),
        translation: [0.5; 3],
        rotation,
    })
}

struct LandOnSave;

impl SaveNormaliser for LandOnSave {
    fn name(&self) -> &'static str {
        ID
    }

    fn normalise(&self, view: &mut SaveView) -> Result<(), String> {
        let Some((chunk, meta)) = view.blocks.as_mut() else {
            return Ok(());
        };
        let carried: Vec<_> = meta
            .raw_block_entities
            .iter()
            .filter(|(_, nbt)| raw::kind(nbt) == Some(BlockEntityType::Piston))
            .filter_map(|(pos, nbt)| {
                let inner = nbt.get_compound(raw::CARRIED)?.clone();
                let state = decode_block_state(nbt.get("blockState")?);
                Some((*pos, state, inner))
            })
            .collect();
        for (pos, state, mut inner) in carried {
            if chunk.get(pos.x, pos.y, pos.z).block() != Block::MovingPiston {
                continue;
            }
            chunk.set(pos.x, pos.y, pos.z, state);
            raw::relocate(&mut inner, pos);
            meta.raw_block_entities.insert(pos, inner);
            meta.relight = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::storage::chunk_nbt::encode_block_state;
    use tuff_nbt::Compound;
    use tuff_world_model::BlockPos;
    use tuff_world_model::chunk::{Chunk, ChunkShape};
    use tuff_world_model::chunk_meta::ChunkMeta;
    use tuff_world_model::pos::ChunkPos;

    #[tuff_core::watchdog]
    #[test]
    fn a_save_mid_move_lands_the_carried_block() {
        let pos = BlockPos::new(3, 70, 4);
        let other = BlockPos::new(5, 70, 4);
        let chest = Block::Chest.default_state();
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        chunk.set(pos.x, pos.y, pos.z, Block::MovingPiston.default_state());
        chunk.set(
            other.x,
            other.y,
            other.z,
            Block::MovingPiston.default_state(),
        );
        let mut meta = ChunkMeta::default();
        let carried = raw::new(BlockEntityType::Chest, BlockPos::new(2, 70, 4)).with("lock", "x");
        let piston = |state| {
            raw::new(BlockEntityType::Piston, pos).with("blockState", encode_block_state(state))
        };
        meta.raw_block_entities
            .insert(pos, piston(chest).with(raw::CARRIED, carried));
        meta.raw_block_entities
            .insert(other, piston(Block::Stone.default_state()));
        let mut view = SaveView {
            dimension: "minecraft:overworld".into(),
            pos: ChunkPos::new(0, 0),
            blocks: Some((chunk, meta)),
            entities: None,
        };
        LandOnSave.normalise(&mut view).unwrap();
        let (chunk, meta) = view.blocks.unwrap();
        assert_eq!(chunk.get(pos.x, pos.y, pos.z), chest);
        let landed: &Compound = &meta.raw_block_entities[&pos];
        assert_eq!(raw::kind(landed), Some(BlockEntityType::Chest));
        assert_eq!(raw::position(landed), Some(pos));
        assert_eq!(landed.get_str("lock"), Some("x"));
        assert_eq!(
            chunk.get(other.x, other.y, other.z).block(),
            Block::MovingPiston,
            "plain moves are left to vanilla"
        );
    }

    fn state(text: &str) -> BlockState {
        text.parse().unwrap()
    }

    fn patterned() -> Compound {
        raw::new(BlockEntityType::Banner, BlockPos::new(0, 70, 0)).with(
            "patterns",
            tuff_nbt::Tag::List(vec![tuff_nbt::Tag::Compound(
                Compound::new()
                    .with("pattern", "minecraft:stripe_top")
                    .with("color", "red"),
            )]),
        )
    }

    #[tuff_core::watchdog]
    #[test]
    fn blocks_drawn_by_their_block_entity_move_as_block_displays() {
        for text in [
            "minecraft:chest[facing=east,type=left]",
            "minecraft:trapped_chest",
            "minecraft:ender_chest",
            "minecraft:copper_chest",
            "minecraft:enchanting_table",
            "minecraft:conduit",
            "minecraft:white_wall_banner",
            "minecraft:red_banner[rotation=3]",
        ] {
            assert_eq!(
                Carrier.look(state(text), None),
                CarriedLook::BlockDisplay,
                "{text}"
            );
        }
        for text in ["minecraft:furnace", "minecraft:hopper", "minecraft:barrel"] {
            assert_eq!(
                Carrier.look(state(text), None),
                CarriedLook::Model,
                "{text}"
            );
        }
        assert_eq!(
            Carrier.look(state("minecraft:white_wall_banner"), Some(&patterned())),
            CarriedLook::BlockDisplay,
            "a wall banner's item would show it standing"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_standing_head_with_a_skin_moves_as_its_item() {
        let nbt = raw::new(BlockEntityType::Skull, BlockPos::new(0, 70, 0))
            .with("profile", Compound::new().with("name", "Steve"));
        let look = Carrier.look(state("minecraft:player_head[rotation=4]"), Some(&nbt));
        let CarriedLook::ItemDisplay { item, .. } = look else {
            panic!("{look:?}");
        };
        assert_eq!(item.item, tuff_vanilla_data::Item::PlayerHead);
        assert!(item.components.other.get("minecraft:profile").is_some());
        assert_eq!(
            Carrier.look(state("minecraft:player_head[rotation=4]"), None),
            CarriedLook::BlockDisplay
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_standing_patterned_banner_moves_as_its_item_turned_like_the_block() {
        let look = Carrier.look(
            state("minecraft:white_banner[rotation=0]"),
            Some(&patterned()),
        );
        let CarriedLook::ItemDisplay {
            item,
            translation,
            rotation,
        } = look
        else {
            panic!("{look:?}");
        };
        assert_eq!(item.item, tuff_vanilla_data::Item::WhiteBanner);
        assert_eq!(item.components.banner_patterns.as_ref().unwrap().len(), 1);
        assert_eq!(translation, [0.5; 3]);
        let expected = [0.0, -1.0, 0.0, 0.0];
        for (got, want) in rotation.iter().zip(expected) {
            assert!((got - want).abs() < 1e-6, "{rotation:?}");
        }
        assert_eq!(
            Carrier.look(
                state("minecraft:white_banner[rotation=0]"),
                Some(&raw::new(BlockEntityType::Banner, BlockPos::new(0, 70, 0)))
            ),
            CarriedLook::BlockDisplay,
            "a plain banner keeps the block's own look"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn signs_move_as_their_item_without_text() {
        for (text, want) in [
            (
                "minecraft:oak_sign[rotation=4]",
                tuff_vanilla_data::Item::OakSign,
            ),
            (
                "minecraft:oak_wall_sign[facing=east]",
                tuff_vanilla_data::Item::OakSign,
            ),
            (
                "minecraft:oak_hanging_sign[rotation=8]",
                tuff_vanilla_data::Item::OakHangingSign,
            ),
            (
                "minecraft:oak_wall_hanging_sign[facing=north]",
                tuff_vanilla_data::Item::OakHangingSign,
            ),
        ] {
            let look = Carrier.look(state(text), None);
            let CarriedLook::ItemDisplay { item, .. } = look else {
                panic!("{text}: {look:?}");
            };
            assert_eq!(item.item, want, "{text}");
        }
    }
}
