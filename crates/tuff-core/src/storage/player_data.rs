//! Player records in vanilla's player-file schema. The whole compound is
//! kept and only the fields this server simulates are overwritten, so
//! advancements-adjacent state, abilities and the like
//! survive untouched.

use std::io;

use tuff_vanilla_data::version::DATA_VERSION;

use crate::protocol::play::GameMode;
use crate::world::effects::ActiveEffects;
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_storage::backend::Documents;
use tuff_storage::document::Document;

/// Vanilla inventory list slots 0-8 are the hotbar, 9-35 the main grid.
const HOTBAR_SLOTS: usize = 9;
const INVENTORY_SLOTS: usize = 36;
pub const ENDER_CHEST_SLOTS: usize = 27;
/// Window slot of vanilla list slot 0 in the player inventory window.
const WINDOW_HOTBAR: usize = 36;
/// `(equipment key, window slot)`.
const EQUIPMENT: [(&str, usize); 5] = [
    ("head", 5),
    ("chest", 6),
    ("legs", 7),
    ("feet", 8),
    ("offhand", 45),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoodSave {
    pub level: i32,
    pub saturation: f32,
    pub exhaustion: f32,
    pub timer: i32,
}

/// The saved recipe book: known recipe names, those still highlighted as
/// new, and `(open, filtering)` per book GUI in the game's order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecipeBookSave {
    pub known: Vec<String>,
    pub highlight: Vec<String>,
    pub settings: [(bool, bool); 4],
}

const RECIPE_BOOK: &str = "recipeBook";
const RECIPE_BOOK_FLAGS: [(&str, &str); 4] = [
    ("isGuiOpen", "isFilteringCraftable"),
    ("isFurnaceGuiOpen", "isFurnaceFilteringCraftable"),
    (
        "isBlastingFurnaceGuiOpen",
        "isBlastingFurnaceFilteringCraftable",
    ),
    ("isSmokerGuiOpen", "isSmokerFilteringCraftable"),
];

impl RecipeBookSave {
    fn from_nbt(root: &Compound) -> Self {
        let Some(book) = root.get_compound(RECIPE_BOOK) else {
            return Self::default();
        };
        let names = |key: &str| -> Vec<String> {
            book.get_list(key)
                .unwrap_or(&[])
                .iter()
                .filter_map(|tag| tag.as_str().map(str::to_owned))
                .collect()
        };
        let mut settings = [(false, false); 4];
        for (setting, (open, filtering)) in settings.iter_mut().zip(RECIPE_BOOK_FLAGS) {
            *setting = (
                book.get_bool(open).unwrap_or(false),
                book.get_bool(filtering).unwrap_or(false),
            );
        }
        Self {
            known: names("recipes"),
            highlight: names("toBeDisplayed"),
            settings,
        }
    }

    fn to_nbt(&self) -> Compound {
        let strings =
            |names: &[String]| Tag::List(names.iter().map(|n| Tag::from(n.as_str())).collect());
        let mut book = Compound::new()
            .with("recipes", strings(&self.known))
            .with("toBeDisplayed", strings(&self.highlight));
        for ((open, filtering), (open_key, filtering_key)) in
            self.settings.iter().zip(RECIPE_BOOK_FLAGS)
        {
            if *open {
                book.insert(open_key, true);
            }
            if *filtering {
                book.insert(filtering_key, true);
            }
        }
        book
    }
}

/// How warned a player has left the sculk; it follows the player across
/// regions and is saved with it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WardenTracker {
    pub ticks_since_last_warning: i32,
    pub warning_level: i32,
    pub cooldown_ticks: i32,
}

impl WardenTracker {
    pub const MAX_WARNING_LEVEL: i32 = 4;
    pub const DECREASE_INTERVAL: i32 = 12_000;
    pub const COOLDOWN: i32 = 200;

    pub fn from_nbt(nbt: &Compound) -> Self {
        let field = |key| nbt.get_int(key).unwrap_or(0).max(0);
        Self {
            ticks_since_last_warning: field("ticks_since_last_warning"),
            warning_level: field("warning_level"),
            cooldown_ticks: field("cooldown_ticks"),
        }
    }

    pub fn to_nbt(self) -> Compound {
        Compound::new()
            .with("ticks_since_last_warning", self.ticks_since_last_warning)
            .with("warning_level", self.warning_level)
            .with("cooldown_ticks", self.cooldown_ticks)
    }

    pub fn tick(&mut self) {
        if self.ticks_since_last_warning >= Self::DECREASE_INTERVAL {
            self.set_warning_level(self.warning_level - 1);
            self.ticks_since_last_warning = 0;
        } else {
            self.ticks_since_last_warning += 1;
        }
        self.cooldown_ticks = (self.cooldown_ticks - 1).max(0);
    }

    pub fn set_warning_level(&mut self, level: i32) {
        self.warning_level = level.clamp(0, Self::MAX_WARNING_LEVEL);
    }

    pub fn on_cooldown(&self) -> bool {
        self.cooldown_ticks > 0
    }

    /// The tracker after one more warning.
    pub fn warned(mut self) -> Self {
        if !self.on_cooldown() {
            self.ticks_since_last_warning = 0;
            self.cooldown_ticks = Self::COOLDOWN;
            self.set_warning_level(self.warning_level + 1);
        }
        self
    }
}

/// Where the player file keeps the entities on the left and right shoulder.
pub const SHOULDER_KEYS: [&str; 2] = ["ShoulderEntityLeft", "ShoulderEntityRight"];

/// The fields of a player file this server simulates.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub dimension: String,
    pub position: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub fall_distance: f64,
    /// Ticks left burning; zero or less when not on fire.
    pub fire: i16,
    /// Ticks of breath left under water.
    pub air: i16,
    /// Ticks spent freezing in powder snow.
    pub ticks_frozen: i32,
    /// Experience points collected, shown on the death screen.
    pub score: i32,
    /// Ticks before a portal can take the player again.
    pub portal_cooldown: i32,
    /// The player has been shown the credits (left the end once).
    pub seen_credits: bool,
    pub warden_tracker: WardenTracker,
    pub health: f32,
    pub food: FoodSave,
    pub selected: usize,
    /// `(window slot, stack)` in the survival inventory window's numbering.
    pub slots: Vec<(usize, ItemStack)>,
    /// `(slot, stack)` of the player's ender chest, slots `0..27`.
    pub ender_items: Vec<(usize, ItemStack)>,
    pub game_mode: GameMode,
    pub previous_game_mode: Option<GameMode>,
    pub flying: bool,
    pub recipe_book: RecipeBookSave,
    pub experience: ExperienceSave,
    /// The player's own spawn point (`/spawnpoint`, a bed).
    pub respawn: Option<RespawnSave>,
    /// Dimension and block position of the last death.
    pub last_death: Option<(String, [i32; 3])>,
    /// Where the player entered the nether, the origin of `nether_travel`.
    pub entered_nether: Option<[f64; 3]>,
    /// Post-process effect ids the client applies.
    pub post_effects: Vec<String>,
    /// The saved entities riding the left and right shoulder.
    pub shoulders: [Option<Compound>; 2],
    pub effects: ActiveEffects,
    pub attributes: crate::world::attributes::AttributeOverrides,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RespawnSave {
    pub dimension: String,
    pub pos: [i32; 3],
    pub yaw: f32,
    pub pitch: f32,
    /// Set by command rather than a bed: it holds without a spawn block.
    pub forced: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ExperienceSave {
    pub level: i32,
    pub progress: f32,
    pub total: i32,
    pub seed: i32,
}

/// A player file as loaded, kept so that saving only overwrites what this
/// server models.
#[derive(Debug, Clone, Default)]
pub struct PlayerRecord {
    raw: Compound,
}

fn window_slot(list_slot: usize) -> Option<usize> {
    match list_slot {
        0..HOTBAR_SLOTS => Some(WINDOW_HOTBAR + list_slot),
        HOTBAR_SLOTS..INVENTORY_SLOTS => Some(list_slot),
        _ => None,
    }
}

fn list_slot(window_slot: usize) -> Option<usize> {
    match window_slot {
        HOTBAR_SLOTS..WINDOW_HOTBAR => Some(window_slot),
        WINDOW_HOTBAR..45 => Some(window_slot - WINDOW_HOTBAR),
        _ => None,
    }
}

fn doubles<const N: usize>(root: &Compound, key: &str) -> Option<[f64; N]> {
    let list = root.get_list(key)?;
    let values: Vec<f64> = list.iter().filter_map(Tag::as_double).collect();
    values.try_into().ok()
}

fn floats<const N: usize>(root: &Compound, key: &str) -> Option<[f32; N]> {
    let list = root.get_list(key)?;
    let values: Vec<f32> = list.iter().filter_map(Tag::as_float).collect();
    values.try_into().ok()
}

pub const MAX_TAGS: usize = 1024;
const WAYPOINT_ICON: &str = "locator_bar_icon";

impl PlayerRecord {
    /// Takes out a key of the player's file core does not simulate, for a
    /// feature that restores what it holds.
    pub fn take_kept(&mut self, key: &str) -> Option<Tag> {
        self.raw.remove(key)
    }

    /// Stores a key core does not simulate in the player's file.
    pub fn keep(&mut self, key: &str, tag: Tag) {
        self.raw.insert(key, tag);
    }

    /// The scoreboard tags `/tag` keeps in the player's file.
    pub fn tags(&self) -> Vec<String> {
        self.raw
            .get_list("Tags")
            .map(|list| {
                list.iter()
                    .filter_map(|tag| tag.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The locator bar icon `/waypoint modify` set.
    pub fn waypoint_icon(&self) -> crate::world::waypoints::Icon {
        let Some(icon) = self.raw.get_compound(WAYPOINT_ICON) else {
            return Default::default();
        };
        crate::world::waypoints::Icon {
            style: icon.get_str("style").map(str::to_owned),
            color: icon.get_int("color").map(|rgb| {
                let [_, r, g, b] = rgb.to_be_bytes();
                [r, g, b]
            }),
        }
    }

    /// Applies a `/waypoint modify`; whether the icon changed.
    pub fn edit_waypoint_icon(&mut self, edit: &crate::world::messages::WaypointEdit) -> bool {
        use crate::world::messages::WaypointEdit;
        let before = self.waypoint_icon();
        let mut icon = before.clone();
        match edit {
            WaypointEdit::Color(color) => icon.color = *color,
            WaypointEdit::Style(style) => icon.style.clone_from(style),
        }
        if icon == before {
            return false;
        }
        let mut nbt = Compound::new();
        if let Some(style) = &icon.style {
            nbt.insert("style", Tag::from(style.as_str()));
        }
        if let Some([r, g, b]) = icon.color {
            nbt.insert("color", Tag::Int(i32::from_be_bytes([0, r, g, b])));
        }
        if nbt.is_empty() {
            self.raw.remove(WAYPOINT_ICON);
        } else {
            self.raw.insert(WAYPOINT_ICON, Tag::Compound(nbt));
        }
        true
    }

    /// Adds or removes one scoreboard tag; whether it changed.
    pub fn edit_tag(&mut self, add: bool, name: &str) -> bool {
        let mut tags = self.tags();
        let changed = if add {
            if tags.len() >= MAX_TAGS || tags.iter().any(|t| t == name) {
                false
            } else {
                tags.push(name.to_owned());
                true
            }
        } else {
            let before = tags.len();
            tags.retain(|t| t != name);
            tags.len() != before
        };
        if changed {
            if tags.is_empty() {
                self.raw.remove("Tags");
            } else {
                self.raw
                    .insert("Tags", Tag::List(tags.into_iter().map(Tag::from).collect()));
            }
        }
        changed
    }

    /// Splits a loaded file into its record and the simulated state, using
    /// `defaults` for anything missing.
    pub fn decode(raw: Compound, defaults: &PlayerState) -> (Self, PlayerState) {
        let slots = stored_slots(&raw);
        let mut ender_items = Vec::new();
        for entry in raw.get_list("EnderItems").unwrap_or(&[]) {
            let Some(entry) = entry.as_compound() else {
                continue;
            };
            let slot = entry.get_byte("Slot").unwrap_or(-1);
            if !(0..ENDER_CHEST_SLOTS as i8).contains(&slot) {
                continue;
            }
            let mut stack = entry.clone();
            stack.remove("Slot");
            if let Some(stack) = ItemStack::from_nbt(&stack) {
                ender_items.push((slot as usize, stack));
            }
        }
        let rotation = floats::<2>(&raw, "Rotation");
        let game_mode = raw
            .get_int("playerGameType")
            .and_then(GameMode::from_id)
            .unwrap_or(defaults.game_mode);
        let state = PlayerState {
            dimension: raw
                .get_str("Dimension")
                .unwrap_or(&defaults.dimension)
                .to_owned(),
            position: doubles(&raw, "Pos")
                .filter(|p: &[f64; 3]| p.iter().all(|c| c.is_finite()))
                .unwrap_or(defaults.position),
            yaw: rotation.map_or(defaults.yaw, |r| r[0]),
            pitch: rotation.map_or(defaults.pitch, |r| r[1]),
            on_ground: raw.get_bool("OnGround").unwrap_or(defaults.on_ground),
            fall_distance: raw.get_double("fall_distance").unwrap_or(0.0),
            fire: raw.get_short("Fire").unwrap_or(0),
            air: raw.get_short("Air").unwrap_or(defaults.air),
            ticks_frozen: raw.get_int("TicksFrozen").unwrap_or(0).max(0),
            score: raw.get_int("Score").unwrap_or(0),
            portal_cooldown: raw.get_int("PortalCooldown").unwrap_or(0),
            seen_credits: raw.get_bool("seenCredits").unwrap_or(false),
            warden_tracker: raw
                .get_compound("warden_spawn_tracker")
                .map(WardenTracker::from_nbt)
                .unwrap_or_default(),
            health: raw.get_float("Health").unwrap_or(defaults.health),
            food: FoodSave {
                level: raw.get_int("foodLevel").unwrap_or(defaults.food.level),
                saturation: raw
                    .get_float("foodSaturationLevel")
                    .unwrap_or(defaults.food.saturation),
                exhaustion: raw
                    .get_float("foodExhaustionLevel")
                    .unwrap_or(defaults.food.exhaustion),
                timer: raw.get_int("foodTickTimer").unwrap_or(defaults.food.timer),
            },
            selected: raw
                .get_int("SelectedItemSlot")
                .filter(|s| (0..HOTBAR_SLOTS as i32).contains(s))
                .unwrap_or(0) as usize,
            slots,
            ender_items,
            game_mode,
            previous_game_mode: raw
                .get_int("previousPlayerGameType")
                .and_then(GameMode::from_id),
            flying: raw
                .get_compound("abilities")
                .and_then(|abilities| abilities.get_bool("flying"))
                .unwrap_or(defaults.flying),
            recipe_book: RecipeBookSave::from_nbt(&raw),
            experience: ExperienceSave {
                level: raw.get_int("XpLevel").unwrap_or(0).max(0),
                progress: raw.get_float("XpP").unwrap_or(0.0).clamp(0.0, 1.0),
                total: raw.get_int("XpTotal").unwrap_or(0).max(0),
                seed: raw.get_int("XpSeed").unwrap_or(0),
            },
            respawn: raw.get_compound("respawn").and_then(|respawn| {
                let &[x, y, z] = respawn.get_int_array("pos")? else {
                    return None;
                };
                Some(RespawnSave {
                    dimension: respawn.get_str("dimension")?.to_owned(),
                    pos: [x, y, z],
                    yaw: respawn.get_float("yaw").unwrap_or(0.0),
                    pitch: respawn.get_float("pitch").unwrap_or(0.0),
                    forced: respawn.get_bool("forced").unwrap_or(false),
                })
            }),
            last_death: raw.get_compound("LastDeathLocation").and_then(|death| {
                let &[x, y, z] = death.get_int_array("pos")? else {
                    return None;
                };
                Some((death.get_str("dimension")?.to_owned(), [x, y, z]))
            }),
            entered_nether: match raw.get_list("entered_nether_pos") {
                Some([Tag::Double(x), Tag::Double(y), Tag::Double(z)]) => Some([*x, *y, *z]),
                _ => None,
            },
            post_effects: raw
                .get_list("post_effects")
                .unwrap_or(&[])
                .iter()
                .filter_map(|tag| match tag {
                    Tag::String(id) => Some(id.to_string()),
                    _ => None,
                })
                .collect(),
            shoulders: SHOULDER_KEYS
                .map(|key| raw.get_compound(key).filter(|tag| !tag.is_empty()).cloned()),
            effects: ActiveEffects::from_nbt(raw.get_list("active_effects").unwrap_or(&[])),
            attributes: crate::world::attributes::AttributeOverrides::from_nbt(
                raw.get_list("attributes").unwrap_or(&[]),
            ),
        };
        (Self { raw }, state)
    }

    pub fn encode(&self, uuid: u128, state: &PlayerState) -> Compound {
        let mut root = self.raw.clone();
        let (inventory, equipment) = slot_lists(&root, &state.slots);
        let ender_items: Vec<Compound> = state
            .ender_items
            .iter()
            .map(|(slot, stack)| {
                let mut nbt = Compound::new().with("Slot", *slot as i8);
                for (key, value) in stack.to_nbt() {
                    nbt.insert(key, value);
                }
                nbt
            })
            .collect();
        let list = |values: Vec<Tag>| Tag::List(values);
        for (key, value) in [
            ("DataVersion", Tag::Int(DATA_VERSION)),
            ("UUID", Tag::uuid(uuid)),
            ("Dimension", Tag::from(state.dimension.as_str())),
            ("Pos", list(state.position.map(Tag::Double).to_vec())),
            (
                "Rotation",
                list(vec![Tag::Float(state.yaw), Tag::Float(state.pitch)]),
            ),
            ("OnGround", Tag::from(state.on_ground)),
            ("fall_distance", Tag::Double(state.fall_distance)),
            ("Fire", Tag::Short(state.fire)),
            ("Air", Tag::Short(state.air)),
            ("PortalCooldown", Tag::Int(state.portal_cooldown)),
            ("seenCredits", Tag::from(state.seen_credits)),
            (
                "warden_spawn_tracker",
                Tag::Compound(state.warden_tracker.to_nbt()),
            ),
            ("Health", Tag::Float(state.health)),
            ("foodLevel", Tag::Int(state.food.level)),
            ("foodSaturationLevel", Tag::Float(state.food.saturation)),
            ("foodExhaustionLevel", Tag::Float(state.food.exhaustion)),
            ("foodTickTimer", Tag::Int(state.food.timer)),
            ("SelectedItemSlot", Tag::Int(state.selected as i32)),
            ("playerGameType", Tag::Int(state.game_mode.id())),
            ("Inventory", Tag::from(inventory)),
            ("EnderItems", Tag::from(ender_items)),
            ("XpLevel", Tag::Int(state.experience.level)),
            ("XpP", Tag::Float(state.experience.progress)),
            ("XpTotal", Tag::Int(state.experience.total)),
            ("XpSeed", Tag::Int(state.experience.seed)),
        ] {
            root.insert(key, value);
        }
        root.insert("Score", state.score);
        if state.ticks_frozen > 0 {
            root.insert("TicksFrozen", state.ticks_frozen);
        } else {
            root.remove("TicksFrozen");
        }
        match state.previous_game_mode {
            Some(mode) => root.insert("previousPlayerGameType", mode.id()),
            None => root.remove("previousPlayerGameType"),
        };
        if state.effects.is_empty() {
            root.remove("active_effects");
        } else {
            root.insert("active_effects", state.effects.to_nbt());
        }
        if state.attributes.is_empty() {
            root.remove("attributes");
        } else {
            root.insert("attributes", Tag::List(state.attributes.to_nbt()));
        }
        match &state.respawn {
            Some(respawn) => root.insert(
                "respawn",
                Compound::new()
                    .with("pos", respawn.pos.to_vec())
                    .with("dimension", respawn.dimension.as_str())
                    .with("yaw", respawn.yaw)
                    .with("pitch", respawn.pitch)
                    .with("forced", respawn.forced),
            ),
            None => root.remove("respawn"),
        };
        match &state.last_death {
            Some((dimension, pos)) => root.insert(
                "LastDeathLocation",
                Compound::new()
                    .with("dimension", dimension.as_str())
                    .with("pos", pos.to_vec()),
            ),
            None => root.remove("LastDeathLocation"),
        };
        match state.entered_nether {
            Some(pos) => root.insert(
                "entered_nether_pos",
                Tag::List(pos.iter().map(|v| Tag::Double(*v)).collect()),
            ),
            None => root.remove("entered_nether_pos"),
        };
        if state.post_effects.is_empty() {
            root.remove("post_effects");
        } else {
            root.insert(
                "post_effects",
                Tag::List(
                    state
                        .post_effects
                        .iter()
                        .map(|id| Tag::from(id.as_str()))
                        .collect(),
                ),
            );
        }
        for (key, shoulder) in SHOULDER_KEYS.iter().zip(&state.shoulders) {
            match shoulder {
                Some(tag) if !tag.is_empty() => root.insert(*key, tag.clone()),
                _ => root.remove(key),
            };
        }
        let mut abilities = root.get_compound("abilities").cloned().unwrap_or_default();
        abilities.insert("flying", state.flying);
        root.insert("abilities", abilities);
        root.insert(RECIPE_BOOK, state.recipe_book.to_nbt());
        if equipment.is_empty() {
            root.remove("equipment");
        } else {
            root.insert("equipment", equipment);
        }
        for (key, value) in [
            ("Motion", list(vec![Tag::Double(0.0); 3])),
            ("Fire", Tag::Short(-20)),
        ] {
            if !root.contains_key(key) {
                root.insert(key, value);
            }
        }
        root
    }
}

/// The survival inventory slots `raw` stores, in window numbering.
fn stored_slots(raw: &Compound) -> Vec<(usize, ItemStack)> {
    let mut slots = Vec::new();
    let mut take = |window: usize, nbt: &Compound| {
        if let Some(stack) = ItemStack::from_nbt(nbt) {
            slots.push((window, stack));
        }
    };
    for entry in raw.get_list("Inventory").unwrap_or(&[]) {
        let Some(entry) = entry.as_compound() else {
            continue;
        };
        let slot = entry.get_byte("Slot").unwrap_or(0) as u8 as usize;
        if let Some(window) = window_slot(slot) {
            let mut stack = entry.clone();
            stack.remove("Slot");
            take(window, &stack);
        }
    }
    if let Some(equipment) = raw.get_compound("equipment") {
        for (key, window) in EQUIPMENT {
            if let Some(stack) = equipment.get_compound(key) {
                take(window, stack);
            }
        }
    }
    slots
}

/// The `Inventory` list and `equipment` compound storing `slots`, keeping
/// equipment entries of `root` this server does not model.
fn slot_lists(root: &Compound, slots: &[(usize, ItemStack)]) -> (Vec<Compound>, Compound) {
    let mut inventory = Vec::new();
    let mut equipment = root.get_compound("equipment").cloned().unwrap_or_default();
    for (key, _) in EQUIPMENT {
        equipment.remove(key);
    }
    for (window, stack) in slots {
        if let Some(slot) = list_slot(*window) {
            let mut nbt = Compound::new().with("Slot", slot as u8 as i8);
            for (key, value) in stack.to_nbt() {
                nbt.insert(key, value);
            }
            inventory.push(nbt);
        } else if let Some((key, _)) = EQUIPMENT.iter().find(|(_, w)| *w == *window) {
            equipment.insert(*key, stack.to_nbt());
        }
    }
    (inventory, equipment)
}

/// Runs `edit` on the inventory stored in `raw` and stores the result
/// when it returns true.
pub fn edit_inventory(
    raw: &mut Compound,
    edit: impl FnOnce(&mut crate::world::inventory::Inventory) -> bool,
) -> bool {
    let mut inventory = crate::world::inventory::Inventory::default();
    for (slot, stack) in stored_slots(raw) {
        inventory.set_slot(slot, Some(stack));
    }
    if let Some(selected) = raw
        .get_int("SelectedItemSlot")
        .filter(|s| (0..HOTBAR_SLOTS as i32).contains(s))
    {
        inventory.set_selected(selected as usize);
    }
    if !edit(&mut inventory) {
        return false;
    }
    let slots: Vec<_> = inventory
        .slots()
        .iter()
        .enumerate()
        .filter_map(|(slot, stack)| stack.clone().map(|s| (slot, s)))
        .collect();
    let (list, equipment) = slot_lists(raw, &slots);
    raw.insert("Inventory", Tag::from(list));
    if equipment.is_empty() {
        raw.remove("equipment");
    } else {
        raw.insert("equipment", equipment);
    }
    true
}

pub fn load(docs: &dyn Documents, uuid: u128) -> io::Result<Option<Compound>> {
    docs.read(Document::Player(uuid))
}

pub fn save(docs: &dyn Documents, uuid: u128, root: Compound) -> io::Result<()> {
    docs.write(Document::Player(uuid), root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Item;

    fn defaults() -> PlayerState {
        PlayerState {
            dimension: "minecraft:overworld".into(),
            position: [0.5, 70.0, 0.5],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
            fall_distance: 0.0,
            fire: 0,
            air: 300,
            ticks_frozen: 0,
            score: 0,
            portal_cooldown: 0,
            seen_credits: false,
            warden_tracker: Default::default(),
            health: 20.0,
            food: FoodSave {
                level: 20,
                saturation: 5.0,
                exhaustion: 0.0,
                timer: 0,
            },
            selected: 0,
            slots: Vec::new(),
            ender_items: Vec::new(),
            game_mode: GameMode::Survival,
            previous_game_mode: None,
            flying: false,
            recipe_book: RecipeBookSave::default(),
            experience: ExperienceSave::default(),
            respawn: None,
            last_death: None,
            entered_nether: None,
            post_effects: Vec::new(),
            shoulders: [None, None],
            effects: ActiveEffects::default(),
            attributes: Default::default(),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn warden_tracker_round_trips_in_the_game_layout() {
        let raw = Compound::new().with(
            "warden_spawn_tracker",
            Compound::new()
                .with("ticks_since_last_warning", 5)
                .with("warning_level", 3)
                .with("cooldown_ticks", 40),
        );
        let (record, state) = PlayerRecord::decode(raw, &defaults());
        assert_eq!(
            state.warden_tracker,
            WardenTracker {
                ticks_since_last_warning: 5,
                warning_level: 3,
                cooldown_ticks: 40
            }
        );
        let saved = record.encode(7, &state);
        let tracker = saved.get_compound("warden_spawn_tracker").unwrap();
        assert_eq!(tracker.get_int("warning_level"), Some(3));
        assert_eq!(tracker.get_int("cooldown_ticks"), Some(40));
        let (_, fresh) = PlayerRecord::decode(Compound::new(), &defaults());
        assert_eq!(fresh.warden_tracker, WardenTracker::default());
    }

    #[tuff_core::watchdog]
    #[test]
    fn warden_tracker_decays_and_warns_with_a_cooldown() {
        let warned = WardenTracker::default().warned();
        assert_eq!((warned.warning_level, warned.cooldown_ticks), (1, 200));
        assert_eq!(warned.warned(), warned, "no second warning on cooldown");
        let mut tracker = warned;
        for _ in 0..200 {
            tracker.tick();
        }
        assert!(!tracker.on_cooldown());
        for _ in 0..=WardenTracker::DECREASE_INTERVAL {
            tracker.tick();
        }
        assert_eq!(tracker.warning_level, 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn seen_credits_round_trip() {
        let raw = Compound::new().with("seenCredits", true);
        let (record, mut state) = PlayerRecord::decode(raw, &defaults());
        assert!(state.seen_credits);
        state.seen_credits = false;
        let saved = record.encode(7, &state);
        assert_eq!(saved.get_bool("seenCredits"), Some(false));
        let (_, fresh) = PlayerRecord::decode(Compound::new(), &defaults());
        assert!(!fresh.seen_credits);
    }

    #[tuff_core::watchdog]
    #[test]
    fn ticks_frozen_round_trip_and_are_left_out_when_thawed() {
        let raw = Compound::new().with("TicksFrozen", 90);
        let (record, mut state) = PlayerRecord::decode(raw, &defaults());
        assert_eq!(state.ticks_frozen, 90);
        let saved = record.encode(7, &state);
        assert_eq!(saved.get_int("TicksFrozen"), Some(90));
        state.ticks_frozen = 0;
        let (record, _) = PlayerRecord::decode(saved, &defaults());
        assert_eq!(record.encode(7, &state).get_int("TicksFrozen"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn score_round_trips() {
        let raw = Compound::new().with("Score", 1234);
        let (record, state) = PlayerRecord::decode(raw, &defaults());
        assert_eq!(state.score, 1234);
        assert_eq!(record.encode(7, &state).get_int("Score"), Some(1234));
    }

    #[tuff_core::watchdog]
    #[test]
    fn last_death_and_post_effects_round_trip() {
        let (record, mut state) = PlayerRecord::decode(Compound::new(), &defaults());
        assert_eq!(state.last_death, None);
        state.last_death = Some(("minecraft:the_nether".into(), [1, 2, 3]));
        state.post_effects = vec!["minecraft:creeper".into()];
        let saved = record.encode(7, &state);
        let (_, back) = PlayerRecord::decode(saved, &defaults());
        assert_eq!(back.last_death, state.last_death);
        assert_eq!(back.post_effects, state.post_effects);
    }

    #[tuff_core::watchdog]
    #[test]
    fn shoulder_entities_round_trip_and_empty_ones_are_dropped() {
        let parrot = Compound::new()
            .with("id", "minecraft:parrot")
            .with("Variant", 3);
        let raw = Compound::new()
            .with("ShoulderEntityLeft", parrot.clone())
            .with("ShoulderEntityRight", Compound::new());
        let (record, mut state) = PlayerRecord::decode(raw, &defaults());
        assert_eq!(state.shoulders, [Some(parrot.clone()), None]);
        let saved = record.encode(7, &state);
        assert_eq!(saved.get_compound("ShoulderEntityLeft"), Some(&parrot));
        assert!(!saved.contains_key("ShoulderEntityRight"));
        state.shoulders = [None, Some(parrot.clone())];
        let saved = record.encode(7, &state);
        assert!(!saved.contains_key("ShoulderEntityLeft"));
        assert_eq!(saved.get_compound("ShoulderEntityRight"), Some(&parrot));
    }

    #[tuff_core::watchdog]
    #[test]
    fn entered_nether_round_trips() {
        let (record, mut state) = PlayerRecord::decode(Compound::new(), &defaults());
        assert_eq!(state.entered_nether, None);
        state.entered_nether = Some([1.5, 64.0, -3.25]);
        let saved = record.encode(7, &state);
        let (_, back) = PlayerRecord::decode(saved, &defaults());
        assert_eq!(back.entered_nether, state.entered_nether);
    }

    #[tuff_core::watchdog]
    #[test]
    fn air_round_trips() {
        let raw = Compound::new().with("Air", 42i16);
        let (record, mut state) = PlayerRecord::decode(raw, &defaults());
        assert_eq!(state.air, 42);
        state.air = -7;
        let saved = record.encode(7, &state);
        assert_eq!(saved.get_short("Air"), Some(-7));
        let (_, reloaded) = PlayerRecord::decode(saved, &defaults());
        assert_eq!(reloaded.air, -7);
        let (_, fresh) = PlayerRecord::decode(Compound::new(), &defaults());
        assert_eq!(fresh.air, 300);
    }

    #[tuff_core::watchdog]
    #[test]
    fn portal_cooldown_round_trips() {
        let raw = Compound::new().with("PortalCooldown", 7);
        let (record, mut state) = PlayerRecord::decode(raw, &defaults());
        assert_eq!(state.portal_cooldown, 7);
        state.portal_cooldown = 3;
        let saved = record.encode(7, &state);
        assert_eq!(saved.get_int("PortalCooldown"), Some(3));
        let (_, fresh) = PlayerRecord::decode(Compound::new(), &defaults());
        assert_eq!(fresh.portal_cooldown, 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn game_mode_and_flight_round_trip() {
        let raw = Compound::new().with("abilities", Compound::new().with("walkSpeed", 0.1f32));
        let (record, mut state) = PlayerRecord::decode(raw, &defaults());
        assert_eq!(state.game_mode, GameMode::Survival);
        assert_eq!(state.previous_game_mode, None);
        state.game_mode = GameMode::Creative;
        state.previous_game_mode = Some(GameMode::Survival);
        state.flying = true;
        state.fire = 57;
        let saved = record.encode(7, &state);
        assert_eq!(saved.get_int("playerGameType"), Some(1));
        assert_eq!(saved.get_short("Fire"), Some(57));
        assert_eq!(saved.get_int("previousPlayerGameType"), Some(0));
        let abilities = saved.get_compound("abilities").unwrap();
        assert_eq!(abilities.get_float("walkSpeed"), Some(0.1));
        let (record, mut reloaded) = PlayerRecord::decode(saved, &defaults());
        assert_eq!(reloaded.game_mode, GameMode::Creative);
        assert_eq!(reloaded.previous_game_mode, Some(GameMode::Survival));
        assert!(reloaded.flying);
        assert_eq!(reloaded.fire, 57);
        reloaded.recipe_book = RecipeBookSave {
            known: vec!["minecraft:piston".into(), "minecraft:bread".into()],
            highlight: vec!["minecraft:bread".into()],
            settings: [(true, false), (false, true), (false, false), (false, false)],
        };
        let saved = record.encode(7, &reloaded);
        let book = saved.get_compound("recipeBook").unwrap();
        assert_eq!(book.get_list("recipes").unwrap().len(), 2);
        assert_eq!(book.get_bool("isGuiOpen"), Some(true));
        assert!(!book.contains_key("isFilteringCraftable"));
        assert_eq!(book.get_bool("isFurnaceFilteringCraftable"), Some(true));
        let (_, again) = PlayerRecord::decode(saved, &defaults());
        assert_eq!(again.recipe_book, reloaded.recipe_book);
        reloaded.previous_game_mode = None;
        let saved = record.encode(7, &reloaded);
        assert!(!saved.contains_key("previousPlayerGameType"));

        let mut spectator = defaults();
        spectator.game_mode = GameMode::Spectator;
        let (_, fresh) = PlayerRecord::decode(Compound::new(), &spectator);
        assert_eq!(fresh.game_mode, GameMode::Spectator);
    }

    #[tuff_core::watchdog]
    #[test]
    fn round_trip_preserves_unknown_fields_and_maps_slots() {
        let raw = Compound::new()
            .with("XpLevel", 30)
            .with(
                "Inventory",
                vec![
                    Compound::new()
                        .with("Slot", 0i8)
                        .with("id", "minecraft:dirt")
                        .with("count", 5),
                    Compound::new()
                        .with("Slot", 20i8)
                        .with("id", "minecraft:stone")
                        .with("count", 1),
                    Compound::new()
                        .with("Slot", 1i8)
                        .with("id", "minecraft:diamond_sword")
                        .with("count", 1)
                        .with("components", Compound::new().with("minecraft:damage", 3)),
                ],
            )
            .with(
                "equipment",
                Compound::new()
                    .with("offhand", ItemStack::new(Item::Torch, 3).to_nbt())
                    .with("body", ItemStack::new(Item::Saddle, 1).to_nbt()),
            )
            .with(
                "EnderItems",
                vec![
                    Compound::new()
                        .with("Slot", 26i8)
                        .with("id", "minecraft:apple")
                        .with("count", 2),
                    Compound::new()
                        .with("Slot", 27i8)
                        .with("id", "minecraft:apple")
                        .with("count", 1),
                ],
            );
        let (record, mut state) = PlayerRecord::decode(raw, &defaults());
        assert_eq!(
            state.ender_items,
            vec![(26, ItemStack::new(Item::Apple, 2))]
        );
        state.ender_items.push((0, ItemStack::new(Item::Stone, 4)));
        assert_eq!(state.position, [0.5, 70.0, 0.5]);
        let slot = |state: &PlayerState, window| {
            state
                .slots
                .iter()
                .find(|(w, _)| *w == window)
                .map(|(_, s)| s.clone())
        };
        assert_eq!(slot(&state, 36), Some(ItemStack::new(Item::Dirt, 5)));
        assert_eq!(slot(&state, 20), Some(ItemStack::new(Item::Stone, 1)));
        let sword = slot(&state, 37).unwrap();
        assert_eq!(sword.item, Item::DiamondSword);
        assert_eq!(sword.damage(), 3);
        assert_eq!(slot(&state, 45), Some(ItemStack::new(Item::Torch, 3)));

        state.position = [10.0, 64.0, -3.0];
        state.slots.retain(|(w, _)| *w != 20);
        for (window, stack) in &mut state.slots {
            if *window == 37 {
                stack.set_damage(4);
            }
        }
        assert!(state.effects.is_empty());
        state
            .effects
            .add(crate::world::effects::ActiveEffect::timed(
                tuff_vanilla_data::registry::MobEffect::NightVision,
                3600,
                0,
            ));
        let saved = record.encode(7, &state);
        let (_, reloaded) = PlayerRecord::decode(saved.clone(), &defaults());
        assert_eq!(reloaded.effects, state.effects);
        assert_eq!(saved.get_int("XpLevel"), Some(30));
        assert_eq!(saved.get_uuid("UUID"), Some(7));
        let inventory = saved.get_list("Inventory").unwrap();
        assert_eq!(inventory.len(), 2);
        let sword = inventory
            .iter()
            .filter_map(Tag::as_compound)
            .find(|c| c.get_byte("Slot") == Some(1))
            .unwrap();
        assert_eq!(
            sword
                .get_compound("components")
                .and_then(|c| c.get_int("minecraft:damage")),
            Some(4)
        );
        let equipment = saved.get_compound("equipment").unwrap();
        assert!(equipment.contains_key("body"));
        assert!(equipment.contains_key("offhand"));

        assert_eq!(saved.get_list("EnderItems").unwrap().len(), 2);

        let (_, reloaded) = PlayerRecord::decode(saved, &defaults());
        assert_eq!(reloaded.position, [10.0, 64.0, -3.0]);
        assert_eq!(reloaded.slots.len(), 3);
        assert_eq!(reloaded.ender_items.len(), 2);
        assert!(
            reloaded
                .ender_items
                .contains(&(0, ItemStack::new(Item::Stone, 4)))
        );
    }
}
