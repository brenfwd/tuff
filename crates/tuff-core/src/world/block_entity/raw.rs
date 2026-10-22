//! Block entities this server keeps as their saved NBT (signs, banners,
//! skulls, bells, beacons, ...): creating them with the game's defaults,
//! what clients receive of them, and the item components they take from
//! the item that places them and give back to their drops.

use std::sync::OnceLock;

use tuff_vanilla_data::registry::{BlockEntityType, DataComponentType};

use tuff_item::item::{Components, ItemStack};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

pub const ID: &str = "id";
pub const COMPONENTS: &str = "components";
const KEEP_PACKED: &str = "keepPacked";
const POSITION_KEYS: [&str; 4] = [ID, "x", "y", "z"];
const ITEMS: &str = "Items";
pub const LOOT_TABLE: &str = "LootTable";
pub const LOOT_TABLE_SEED: &str = "LootTableSeed";
pub const LOCK: &str = "lock";
const IS_WAXED: &str = "is_waxed";
/// Components an item carries for its block that never reach the block
/// entity's own `components`.
const PLACEMENT_COMPONENTS: [DataComponentType; 2] = [
    DataComponentType::BlockEntityData,
    DataComponentType::BlockState,
];

pub fn kind(nbt: &Compound) -> Option<BlockEntityType> {
    BlockEntityType::from_name(nbt.get_str(ID)?)
}

pub fn position(nbt: &Compound) -> Option<BlockPos> {
    Some(BlockPos::new(
        nbt.get_int("x")?,
        nbt.get_int("y")?,
        nbt.get_int("z")?,
    ))
}

/// What a new block entity of `kind` saves, without id and position.
fn default_data(kind: BlockEntityType) -> &'static Compound {
    static DEFAULTS: OnceLock<Vec<Compound>> = OnceLock::new();
    let defaults = DEFAULTS.get_or_init(|| {
        BlockEntityType::ALL
            .iter()
            .map(|kind| {
                kind.default_snbt()
                    .and_then(|snbt| Tag::from_snbt(snbt).ok())
                    .and_then(|tag| tag.as_compound().cloned())
                    .unwrap_or_default()
            })
            .collect()
    });
    &defaults[kind.id() as usize]
}

/// A new block entity of `kind` at `pos`, as saved.
pub fn new(kind: BlockEntityType, pos: BlockPos) -> Compound {
    let mut nbt = Compound::new()
        .with(ID, kind.name())
        .with("x", pos.x)
        .with("y", pos.y)
        .with("z", pos.z)
        .with(KEEP_PACKED, false);
    for (key, value) in default_data(kind).iter() {
        nbt.insert(key, value.clone());
    }
    nbt
}

/// What clients receive of a block entity type's saved data.
enum ClientData {
    Nothing,
    /// Every saved field except the block entity's `components`.
    Saved,
    /// Every saved field.
    SavedWithComponents,
    Only(&'static [&'static str]),
}

fn client_data_kind(kind: BlockEntityType) -> ClientData {
    use BlockEntityType::*;
    match kind {
        Sign | HangingSign | Skull | DecoratedPot | Beacon | Conduit | MobSpawner | Shelf
        | CreakingHeart | Jigsaw | StructureBlock | EndGateway | TestBlock | TestInstanceBlock
        | Piston => ClientData::Saved,
        Banner => ClientData::SavedWithComponents,
        BrushableBlock => ClientData::Only(&["hit_direction", "item"]),
        TrialSpawner => ClientData::Only(&["spawn_data", "next_mob_spawns_at"]),
        Vault => ClientData::Only(&["shared_data"]),
        Campfire => ClientData::Only(&[ITEMS]),
        _ => ClientData::Nothing,
    }
}

/// The block entity a moving piston carries to where its block lands.
pub const CARRIED: &str = "tuff:carried";

/// Fields the client never sees.
const SERVER_ONLY_KEYS: [&str; 3] = [KEEP_PACKED, "SpawnPotentials", CARRIED];

/// Moves saved block entity data to `pos`.
pub fn relocate(nbt: &mut Compound, pos: BlockPos) {
    nbt.insert("x", pos.x);
    nbt.insert("y", pos.y);
    nbt.insert("z", pos.z);
}

/// The client data of a saved block entity: position, type id and NBT.
pub fn client_data(nbt: &Compound) -> Option<(BlockPos, BlockEntityType, Compound)> {
    let kind = kind(nbt)?;
    let pos = position(nbt)?;
    let mut data = Compound::new();
    let sent = |key: &str| match client_data_kind(kind) {
        ClientData::Nothing => false,
        ClientData::Saved => {
            !POSITION_KEYS.contains(&key) && !SERVER_ONLY_KEYS.contains(&key) && key != COMPONENTS
        }
        ClientData::SavedWithComponents => {
            !POSITION_KEYS.contains(&key) && !SERVER_ONLY_KEYS.contains(&key)
        }
        ClientData::Only(keys) => keys.contains(&key),
    };
    for (key, value) in nbt.iter() {
        if sent(key) {
            data.insert(key, value.clone());
        }
    }
    Some((pos, kind, data))
}

/// Where a block entity keeps one of its item components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// Saved under the key exactly as the component is.
    Same(&'static str),
    /// A `container` as the slot-numbered `Items` list.
    Items,
    /// A `container`'s first slot as one item under the key.
    SingleItem(&'static str),
    /// A `container_loot` as `LootTable` and `LootTableSeed`.
    LootTable,
    /// `waxed` as the `is_waxed` flag.
    Waxed,
}

impl Field {
    fn keys(self) -> Vec<&'static str> {
        match self {
            Self::Same(key) | Self::SingleItem(key) => vec![key],
            Self::Items => vec![ITEMS],
            Self::LootTable => vec![LOOT_TABLE, LOOT_TABLE_SEED],
            Self::Waxed => vec![IS_WAXED],
        }
    }
}

fn field(kind: BlockEntityType, component: DataComponentType) -> Option<Field> {
    use DataComponentType as C;
    Some(match component {
        C::CustomName if kind == BlockEntityType::Skull => Field::Same("custom_name"),
        C::CustomName => Field::Same("CustomName"),
        C::Lock => Field::Same(LOCK),
        C::Container if kind == BlockEntityType::DecoratedPot => Field::SingleItem("item"),
        C::Container => Field::Items,
        C::ContainerLoot => Field::LootTable,
        C::BannerPatterns => Field::Same("patterns"),
        C::Profile => Field::Same("profile"),
        C::NoteBlockSound => Field::Same("note_block_sound"),
        C::Bees => Field::Same("bees"),
        C::PotDecorations => Field::Same("sherds"),
        C::SignTextFront => Field::Same("front_text"),
        C::SignTextBack => Field::Same("back_text"),
        C::Waxed => Field::Waxed,
        _ => return None,
    })
}

fn reset(kind: BlockEntityType, nbt: &mut Compound, field: Field) {
    for key in field.keys() {
        match default_data(kind).get(key) {
            Some(value) => nbt.insert(key, value.clone()),
            None => nbt.remove(key),
        };
    }
}

fn set(kind: BlockEntityType, nbt: &mut Compound, field: Field, value: &Tag) {
    match field {
        Field::Same(key) => {
            nbt.insert(key, value.clone());
        }
        Field::Items => {
            let size = kind.container_size().unwrap_or(0);
            let items: Vec<Tag> = value
                .as_list()
                .unwrap_or(&[])
                .iter()
                .filter_map(|entry| {
                    let entry = entry.as_compound()?;
                    let slot = entry.get_int("slot")?;
                    if !(0..size as i32).contains(&slot) {
                        return None;
                    }
                    let mut item = Compound::new().with("Slot", slot as i8);
                    for (key, value) in entry.get_compound("item")?.iter() {
                        item.insert(key, value.clone());
                    }
                    Some(Tag::Compound(item))
                })
                .collect();
            nbt.insert(ITEMS, Tag::List(items));
        }
        Field::SingleItem(key) => {
            let first = value.as_list().unwrap_or(&[]).iter().find_map(|entry| {
                let entry = entry.as_compound()?;
                (entry.get_int("slot")? == 0).then(|| entry.get_compound("item").cloned())?
            });
            match first {
                Some(item) => nbt.insert(key, item),
                None => nbt.remove(key),
            };
        }
        Field::LootTable => {
            let Some(loot) = value.as_compound() else {
                return;
            };
            match loot.get_str("loot_table") {
                Some(table) => nbt.insert(LOOT_TABLE, table),
                None => nbt.remove(LOOT_TABLE),
            };
            match loot.get("seed").cloned() {
                Some(seed) if seed != Tag::Long(0) => nbt.insert(LOOT_TABLE_SEED, seed),
                _ => nbt.remove(LOOT_TABLE_SEED),
            };
        }
        Field::Waxed => {
            nbt.insert(IS_WAXED, true);
        }
    }
}

/// The component a field holds, or `None` when it holds the default.
fn get(kind: BlockEntityType, nbt: &Compound, field: Field) -> Option<Tag> {
    let is_default = |key: &str, value: &Tag| {
        matches!(value, Tag::List(list) if list.is_empty())
            || default_data(kind).get(key) == Some(value)
    };
    match field {
        Field::Same(key) => {
            let value = nbt.get(key)?;
            (!is_default(key, value)).then(|| value.clone())
        }
        Field::Items => {
            let items: Vec<Tag> = nbt
                .get_list(ITEMS)?
                .iter()
                .filter_map(|entry| {
                    let entry = entry.as_compound()?;
                    let slot = entry.get_byte("Slot")?;
                    let mut item = entry.clone();
                    item.remove("Slot");
                    ItemStack::picture_from_nbt(&item).map(drop)?;
                    Some(Tag::Compound(
                        Compound::new().with("slot", slot as i32).with("item", item),
                    ))
                })
                .collect();
            (!items.is_empty()).then_some(Tag::List(items))
        }
        Field::SingleItem(key) => {
            let item = nbt.get_compound(key)?;
            ItemStack::picture_from_nbt(item).map(drop)?;
            Some(Tag::List(vec![Tag::Compound(
                Compound::new().with("slot", 0).with("item", item.clone()),
            )]))
        }
        Field::LootTable => {
            let table = nbt.get_str(LOOT_TABLE)?;
            let mut loot = Compound::new().with("loot_table", table);
            if let Some(seed) = nbt.get(LOOT_TABLE_SEED).filter(|s| **s != Tag::Long(0)) {
                loot.insert("seed", seed.clone());
            }
            Some(Tag::Compound(loot))
        }
        Field::Waxed => nbt
            .get_bool(IS_WAXED)
            .filter(|waxed| *waxed)
            .map(|_| Tag::Compound(Compound::new())),
    }
}

/// Applies the item components of the item that placed a block entity:
/// those it models go into its fields (fields the item leaves unset go
/// back to their defaults, except a loot table), the rest into its
/// `components`.
pub fn apply_components(kind: BlockEntityType, nbt: &mut Compound, components: &Components) {
    let patch = components.to_nbt();
    let implicit = kind.implicit_components();
    for component in implicit {
        let Some(field) = field(kind, *component) else {
            continue;
        };
        match patch.get(component.name()) {
            Some(value) => set(kind, nbt, field, value),
            None if *component != DataComponentType::ContainerLoot => reset(kind, nbt, field),
            None => {}
        }
    }
    let mut rest = Compound::new();
    for (key, value) in patch.iter() {
        let kept = DataComponentType::from_name(key).is_none_or(|component| {
            !implicit.contains(&component) && !PLACEMENT_COMPONENTS.contains(&component)
        });
        if !key.starts_with('!') && kept {
            rest.insert(key, value.clone());
        }
    }
    nbt.insert(COMPONENTS, rest);
}

/// The item components a block entity gives the item that represents it:
/// its `components` and whatever of its fields hold more than defaults,
/// as stored component NBT by id.
pub fn collect_components(kind: BlockEntityType, nbt: &Compound) -> Compound {
    let mut collected = nbt.get_compound(COMPONENTS).cloned().unwrap_or_default();
    for component in kind.implicit_components() {
        if let Some(field) = field(kind, *component)
            && let Some(value) = get(kind, nbt, field)
        {
            collected.insert(component.name(), value);
        }
    }
    collected
}

/// The components a creative ctrl-pick copies from a saved block entity
/// into the picked item: the collected components plus
/// `block_entity_data` holding the rest of its own data (without position,
/// its `components` and the keys those components came from). Returns
/// stored component NBT by id.
pub fn picked_components(kind: BlockEntityType, nbt: &Compound) -> Compound {
    let mut data = Compound::new();
    for (key, value) in nbt.iter() {
        let skipped = POSITION_KEYS.contains(&key)
            || key == COMPONENTS
            || key == KEEP_PACKED
            || kind.component_keys().contains(&key);
        if !skipped {
            data.insert(key, value.clone());
        }
    }
    let mut components = Compound::new();
    if !data.is_empty() {
        data.insert(ID, kind.name());
        components.insert(DataComponentType::BlockEntityData.name(), data);
    }
    for (key, value) in collect_components(kind, nbt).iter() {
        components.insert(key, value.clone());
    }
    components
}

/// Merges an item's `block_entity_data` into a placed block entity, as the
/// game loads it over the block entity's own data. Returns whether
/// anything changed.
pub fn apply_block_entity_data(nbt: &mut Compound, data: &Compound) -> bool {
    let before = nbt.clone();
    for (key, value) in data.iter() {
        if POSITION_KEYS.contains(&key) || key == COMPONENTS || key == KEEP_PACKED {
            continue;
        }
        merge(nbt, key, value);
    }
    *nbt != before
}

/// A command's block entity data loaded over the block entity `nbt` the
/// way the game reloads a block entity: every field the data leaves out
/// goes back to its default (`chest{}` empties the chest), and nested
/// compounds fill in their own defaults. `None` if `nbt` has no known type
/// or position.
pub fn reload_block_entity_data(nbt: &Compound, data: &Compound) -> Option<Compound> {
    let mut fresh = new(kind(nbt)?, position(nbt)?);
    for (key, value) in data.iter() {
        if !POSITION_KEYS.contains(&key) && key != KEEP_PACKED {
            merge(&mut fresh, key, value);
        }
    }
    Some(fresh)
}

fn merge(into: &mut Compound, key: &str, value: &Tag) {
    match (into.get_compound_mut(key), value.as_compound()) {
        (Some(current), Some(value)) => {
            for (inner, inner_value) in value.iter() {
                merge(current, inner, inner_value);
            }
        }
        _ => {
            into.insert(key, value.clone());
        }
    }
}

/// The items a block entity holds that spill out when its block goes.
pub fn contents(kind: BlockEntityType, nbt: &Compound) -> Vec<ItemStack> {
    if kind == BlockEntityType::Piston {
        return nbt
            .get_compound(CARRIED)
            .and_then(|carried| Some(contents(self::kind(carried)?, carried)))
            .unwrap_or_default();
    }
    if kind == BlockEntityType::ShulkerBox {
        return Vec::new();
    }
    let mut stacks = Vec::new();
    for component in kind.implicit_components() {
        if *component != DataComponentType::Container {
            continue;
        }
        let Some(field) = field(kind, *component) else {
            continue;
        };
        let Some(Tag::List(entries)) = get(kind, nbt, field) else {
            continue;
        };
        stacks.extend(entries.iter().filter_map(|entry| {
            ItemStack::from_storage(entry.as_compound()?.get_compound("item")?)
        }));
    }
    stacks
}

/// The single item a jukebox or lectern holds outside any container
/// component, popped out when its block goes: a jukebox's record from a
/// random spot just above the block, a lectern's book a quarter block toward
/// its facing (only while the state says it has one). Both get the usual
/// fresh item velocity.
pub fn held_drop(
    kind: BlockEntityType,
    nbt: &Compound,
    state: tuff_vanilla_data::BlockState,
    pos: BlockPos,
    rng: &mut impl rand::Rng,
) -> Option<(ItemStack, [f64; 3], [f64; 3])> {
    let (x, y, z) = (pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5);
    let (stack, position) = match kind {
        BlockEntityType::Jukebox => {
            let stack = ItemStack::from_storage(nbt.get_compound("RecordItem")?)?;
            let dx = (rng.r#gen::<f32>() - 0.5) * 0.7;
            let dz = (rng.r#gen::<f32>() - 0.5) * 0.7;
            (stack, [x + dx as f64, y + 1.01, z + dz as f64])
        }
        BlockEntityType::Lectern => {
            if state.get("has_book") != Some("true") {
                return None;
            }
            let stack = ItemStack::from_storage(nbt.get_compound("Book")?)?;
            let (sx, sz) = match state.get("facing")? {
                "north" => (0.0, -0.25),
                "south" => (0.0, 0.25),
                "west" => (-0.25, 0.0),
                "east" => (0.25, 0.0),
                _ => (0.0, 0.0),
            };
            (stack, [x + sx, y + 1.0, z + sz])
        }
        _ => return None,
    };
    let velocity = [
        rng.r#gen::<f64>() * 0.2 - 0.1,
        0.2,
        rng.r#gen::<f64>() * 0.2 - 0.1,
    ];
    Some((stack, position, velocity))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::decor::BannerLayer;
    use tuff_item::item::{ComponentPatch, MintReason};
    use tuff_item::text::TextComponent;
    use tuff_vanilla_data::Item;
    use tuff_vanilla_data::item::DyeColor;

    #[tuff_core::watchdog]
    #[test]
    fn a_moving_piston_hides_what_it_carries_but_spills_it() {
        let chest = new(BlockEntityType::Chest, BlockPos::new(1, 2, 3)).with(
            ITEMS,
            Tag::List(vec![Tag::Compound(
                Compound::new()
                    .with("Slot", Tag::Byte(0))
                    .with("id", "minecraft:diamond")
                    .with("count", Tag::Int(4)),
            )]),
        );
        let piston = new(BlockEntityType::Piston, BlockPos::new(2, 2, 3)).with(CARRIED, chest);
        let (_, _, data) = client_data(&piston).unwrap();
        assert!(!data.contains_key(CARRIED));
        let spilled = contents(BlockEntityType::Piston, &piston);
        assert_eq!(spilled.len(), 1);
        assert_eq!(spilled[0].item(), Item::Diamond);
        assert_eq!(spilled[0].count(), 4);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_raw_shulker_box_keeps_its_contents_for_the_item() {
        let shulker = new(BlockEntityType::ShulkerBox, BlockPos::new(1, 2, 3)).with(
            ITEMS,
            Tag::List(vec![Tag::Compound(
                Compound::new()
                    .with("Slot", Tag::Byte(0))
                    .with("id", "minecraft:diamond")
                    .with("count", Tag::Int(4)),
            )]),
        );
        assert!(contents(BlockEntityType::ShulkerBox, &shulker).is_empty());
        let piston = new(BlockEntityType::Piston, BlockPos::new(2, 2, 3)).with(CARRIED, shulker);
        assert!(contents(BlockEntityType::Piston, &piston).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn lecterns_drop_their_book_only_while_the_state_shows_it() {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let nbt = Compound::new().with(
            "Book",
            Compound::new()
                .with("id", "minecraft:written_book")
                .with("count", Tag::Int(1)),
        );
        let pos = BlockPos::new(3, 64, -2);
        let empty = tuff_vanilla_data::Block::Lectern
            .default_state()
            .with("facing", "south")
            .unwrap();
        let full = empty.with("has_book", "true").unwrap();
        let kind = BlockEntityType::Lectern;
        assert!(held_drop(kind, &nbt, empty, pos, &mut rng).is_none());
        let (stack, at, velocity) = held_drop(kind, &nbt, full, pos, &mut rng).unwrap();
        assert_eq!(stack.item(), Item::WrittenBook);
        assert_eq!(at, [3.5, 65.0, -1.25]);
        assert_eq!(velocity[1], 0.2);
        assert!(velocity[0].abs() <= 0.1 && velocity[2].abs() <= 0.1);
        assert!(held_drop(BlockEntityType::Chest, &nbt, full, pos, &mut rng).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_default_parses_and_matches_what_clients_get() {
        for kind in BlockEntityType::ALL {
            let Some(snbt) = kind.default_snbt() else {
                continue;
            };
            let default = Tag::from_snbt(snbt)
                .unwrap_or_else(|e| panic!("{kind:?}: {e}"))
                .as_compound()
                .cloned()
                .unwrap();
            assert_eq!(default_data(kind), &default);
            let pos = BlockPos::new(1, 2, 3);
            let nbt = new(kind, pos);
            let (at, listed, data) = client_data(&nbt).unwrap();
            assert_eq!((at, listed), (pos, kind));
            let expected = Tag::from_snbt(kind.default_client_snbt().unwrap()).unwrap();
            assert_eq!(Tag::Compound(data), expected, "{kind:?}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_implicit_component_has_a_field_among_the_types_keys() {
        for kind in BlockEntityType::ALL {
            for component in kind.implicit_components() {
                let field = field(kind, *component)
                    .unwrap_or_else(|| panic!("{kind:?} takes {component:?}"));
                for key in field.keys() {
                    assert!(
                        kind.component_keys().contains(&key),
                        "{kind:?} {component:?} {key}"
                    );
                }
                assert!(!field.keys().is_empty(), "{kind:?} {component:?}");
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn banner_patterns_and_names_move_between_item_and_block_entity() {
        let kind = BlockEntityType::Banner;
        let components = Components::from(ComponentPatch {
            banner_patterns: Some(vec![BannerLayer {
                inline: None,
                pattern: "minecraft:stripe_top".into(),
                color: DyeColor::from_name("red").unwrap(),
            }]),
            custom_name: Some(Box::new(TextComponent::new("Flag"))),
            other: Compound::new().with("minecraft:rarity", "epic"),
            ..Default::default()
        });
        let mut nbt = new(kind, BlockPos::new(0, 64, 0));
        apply_components(kind, &mut nbt, &components);
        assert_eq!(nbt.get_list("patterns").unwrap().len(), 1);
        assert_eq!(nbt.get_str("CustomName"), Some("Flag"));
        assert_eq!(
            nbt.get_compound(COMPONENTS),
            Some(&Compound::new().with("minecraft:rarity", "epic"))
        );
        let (_, _, client) = client_data(&nbt).unwrap();
        assert!(client.contains_key("patterns"));
        assert!(client.contains_key(COMPONENTS));
        let collected = collect_components(kind, &nbt);
        assert_eq!(Components::from_nbt(&collected), components);
        assert!(collect_components(kind, &new(kind, BlockPos::default())).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn containers_skulls_signs_and_loot_map_to_their_fields() {
        let mut shelf = new(BlockEntityType::ChiseledBookshelf, BlockPos::default());
        let components = Components::from(ComponentPatch {
            container: Some(vec![
                Some(ItemStack::mint(Item::Book, 1, MintReason::Test)),
                None,
                None,
                None,
                None,
                None,
                Some(ItemStack::mint(Item::Book, 1, MintReason::Test)),
            ]),
            ..Default::default()
        });
        apply_components(BlockEntityType::ChiseledBookshelf, &mut shelf, &components);
        assert_eq!(
            shelf.get_list(ITEMS).unwrap().len(),
            1,
            "slot 6 is past the end"
        );
        assert_eq!(
            contents(BlockEntityType::ChiseledBookshelf, &shelf),
            vec![ItemStack::mint(Item::Book, 1, MintReason::Test)]
        );

        let mut skull = new(BlockEntityType::Skull, BlockPos::default());
        let profile = Compound::new().with("name", "Steve");
        let components = Components::from(ComponentPatch {
            custom_name: Some(Box::new(TextComponent::new("Head"))),
            other: Compound::new()
                .with("minecraft:profile", profile.clone())
                .with("minecraft:note_block_sound", "minecraft:entity.pig.ambient"),
            ..Default::default()
        });
        apply_components(BlockEntityType::Skull, &mut skull, &components);
        assert_eq!(skull.get_compound("profile"), Some(&profile));
        assert_eq!(
            skull.get_str("note_block_sound"),
            Some("minecraft:entity.pig.ambient")
        );
        assert_eq!(skull.get_str("custom_name"), Some("Head"));
        assert_eq!(skull.get_compound(COMPONENTS), Some(&Compound::new()));

        let mut sign = new(BlockEntityType::Sign, BlockPos::default());
        assert!(collect_components(BlockEntityType::Sign, &sign).is_empty());
        let waxed = Components::from(ComponentPatch {
            other: Compound::new().with("minecraft:waxed", Compound::new()),
            ..Default::default()
        });
        apply_components(BlockEntityType::Sign, &mut sign, &waxed);
        assert_eq!(sign.get_bool(IS_WAXED), Some(true));
        assert_eq!(
            collect_components(BlockEntityType::Sign, &sign),
            Compound::new().with("minecraft:waxed", Compound::new())
        );

        let mut chest = new(BlockEntityType::Chest, BlockPos::default());
        let loot = Compound::new()
            .with("loot_table", "minecraft:chests/simple_dungeon")
            .with("seed", 5i64);
        let components = Components::from(ComponentPatch {
            other: Compound::new().with("minecraft:container_loot", loot.clone()),
            ..Default::default()
        });
        apply_components(BlockEntityType::Chest, &mut chest, &components);
        assert_eq!(
            chest.get_str(LOOT_TABLE),
            Some("minecraft:chests/simple_dungeon")
        );
        assert_eq!(chest.get(LOOT_TABLE_SEED), Some(&Tag::Long(5)));
        assert_eq!(
            collect_components(BlockEntityType::Chest, &chest)
                .get_compound("minecraft:container_loot"),
            Some(&loot)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn block_entity_data_merges_over_the_placed_block_entity() {
        let mut sign = new(BlockEntityType::Sign, BlockPos::new(4, 5, 6));
        let data = Compound::new()
            .with(ID, "minecraft:sign")
            .with("x", 99)
            .with(
                "front_text",
                Compound::new().with(
                    "messages",
                    vec![Tag::from("a"), "b".into(), "".into(), "".into()],
                ),
            );
        assert!(apply_block_entity_data(&mut sign, &data));
        assert_eq!(position(&sign), Some(BlockPos::new(4, 5, 6)));
        let front = sign.get_compound("front_text").unwrap();
        assert_eq!(front.get_str("color"), Some("black"));
        assert_eq!(front.get_list("messages").unwrap()[0], Tag::from("a"));
        assert!(!apply_block_entity_data(&mut sign, &data));
    }

    #[tuff_core::watchdog]
    #[test]
    fn command_data_reloads_every_field_from_defaults() {
        let mut sign = new(BlockEntityType::Sign, BlockPos::new(4, 5, 6));
        sign.insert("is_waxed", true);
        let data = Compound::new().with("x", 99).with(
            "front_text",
            Compound::new().with(
                "messages",
                vec![Tag::from("a"), "b".into(), "".into(), "".into()],
            ),
        );
        let reloaded = reload_block_entity_data(&sign, &data).unwrap();
        assert_eq!(position(&reloaded), Some(BlockPos::new(4, 5, 6)));
        assert_eq!(reloaded.get("is_waxed"), Some(&Tag::Byte(0)));
        let front = reloaded.get_compound("front_text").unwrap();
        assert_eq!(front.get_str("color"), Some("black"));
        assert_eq!(front.get_list("messages").unwrap()[0], Tag::from("a"));
    }
}
