// SPDX-License-Identifier: MIT
//! Fluent handles over the host's world calls: a [`World`] per dimension,
//! [`EntityRef`] for a loaded entity, [`PlayerExt`] on players and
//! [`acting`] for whoever the current callback acts for.
//!
//! ```ignore
//! use tuff_plugin_sdk::world::{PlayerExt, World, acting, pos};
//!
//! let overworld = World::overworld();
//! if overworld.block(pos(0, 64, 0))? == "minecraft:air" {
//!     overworld.set_block(pos(0, 64, 0), "minecraft:stone")?;
//! }
//! acting::give("minecraft:bread", 3)?;
//! player.tell("hi")?;
//! ```
//!
//! World reads and writes are bounded per call by the host; errors say
//! which limit was hit.

use crate::bindings::tuff::plugin::host;
use crate::reply::Replier;
use crate::text::Span;
use crate::types::{
    BlockBox, BlockPos, DustOptions, DustTransitionOptions, Entity, ParticleOptions, ParticleSpawn,
    Player, Rotation, SetFlags, Traits, Transform, Uuid, Vec3, WorldError,
};

pub const OVERWORLD: &str = "minecraft:overworld";
pub const NETHER: &str = "minecraft:the_nether";
pub const END: &str = "minecraft:the_end";

pub const fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos { x, y, z }
}

pub const fn vec3(x: f64, y: f64, z: f64) -> Vec3 {
    Vec3 { x, y, z }
}

/// Result of [`World::blocks`]: a palette plus indices in
/// the host's order, with 65535 marking a block outside the world.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockGrid {
    found: BlockBox,
    a: BlockPos,
    b: BlockPos,
}

impl BlockGrid {
    pub fn palette(&self) -> &[String] {
        &self.found.palette
    }

    pub fn indices(&self) -> &[u16] {
        &self.found.indices
    }

    pub fn len(&self) -> usize {
        self.found.indices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.found.indices.is_empty()
    }

    pub fn corners(&self) -> (BlockPos, BlockPos) {
        (self.a, self.b)
    }

    /// The state at flat index `i`; None past the end or outside the world.
    pub fn get(&self, i: usize) -> Option<&str> {
        let index = *self.found.indices.get(i)?;
        self.found
            .palette
            .get(usize::from(index))
            .map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = Option<&str>> {
        (0..self.len()).map(|i| self.get(i))
    }
}

/// A block state string with registry helpers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BlockState(String);

impl BlockState {
    pub fn new(state: impl Into<String>) -> Self {
        Self(state.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The full `namespace:name[props]` form, defaults filled in.
    pub fn normalize(&self) -> Result<BlockState, RegistryError> {
        host::block_normalize(&self.0)
            .map(Self)
            .ok_or(RegistryError::Unknown)
    }

    /// The block's properties as (name, allowed values).
    pub fn properties(&self) -> Result<Vec<(String, Vec<String>)>, RegistryError> {
        host::block_properties(&self.0)
            .map(|list| list.into_iter().map(|p| (p.name, p.values)).collect())
            .ok_or(RegistryError::Unknown)
    }

    pub fn transform(&self, op: BlockTransform) -> Result<BlockState, RegistryError> {
        host::block_transform(&self.0, op.into())
            .map(Self)
            .ok_or(RegistryError::Unknown)
    }

    pub fn traits(&self) -> Result<Traits, RegistryError> {
        host::block_traits(&self.0).ok_or(RegistryError::Unknown)
    }

    pub fn is_solid(&self) -> Result<bool, RegistryError> {
        Ok(self.traits()?.contains(Traits::SOLID))
    }

    pub fn is_air(&self) -> Result<bool, RegistryError> {
        Ok(self.traits()?.contains(Traits::AIR))
    }

    pub fn is_liquid(&self) -> Result<bool, RegistryError> {
        Ok(self.traits()?.contains(Traits::LIQUID))
    }

    pub fn is_replaceable(&self) -> Result<bool, RegistryError> {
        Ok(self.traits()?.contains(Traits::REPLACEABLE))
    }
}

impl std::fmt::Display for BlockState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for BlockState {
    fn from(state: &str) -> Self {
        Self::new(state)
    }
}

/// Member block names of a tag (`#` optional); unknown or empty is an error.
pub fn block_tag(tag: &str) -> Result<Vec<String>, RegistryError> {
    host::block_tag(tag).ok_or(RegistryError::Unknown)
}

/// Why a registry query failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// Unknown block, property, value or tag.
    Unknown,
    /// The answer did not fit the buffer.
    Limit,
}

/// A rotation or mirror; rotations are clockwise from above.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockTransform {
    Rotate90,
    Rotate180,
    Rotate270,
    SwapNorthSouth,
    SwapEastWest,
}

impl From<BlockTransform> for Transform {
    fn from(op: BlockTransform) -> Self {
        match op {
            BlockTransform::Rotate90 => Transform::Rotate90,
            BlockTransform::Rotate180 => Transform::Rotate180,
            BlockTransform::Rotate270 => Transform::Rotate270,
            BlockTransform::SwapNorthSouth => Transform::SwapNorthSouth,
            BlockTransform::SwapEastWest => Transform::SwapEastWest,
        }
    }
}

/// One dimension, by key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct World {
    key: String,
}

impl World {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }

    pub fn overworld() -> Self {
        Self::new(OVERWORLD)
    }

    pub fn nether() -> Self {
        Self::new(NETHER)
    }

    pub fn end() -> Self {
        Self::new(END)
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// The block state at `at`, such as `minecraft:oak_log[axis=y]`.
    pub fn block(&self, at: BlockPos) -> Result<String, WorldError> {
        host::block_get(&self.key, at)
    }

    /// Every block in the inclusive box between `a` and `b`.
    pub fn blocks(&self, a: BlockPos, b: BlockPos) -> Result<BlockGrid, WorldError> {
        host::blocks_get(&self.key, a, b).map(|found| BlockGrid { found, a, b })
    }

    /// The blocks the world generator makes in the inclusive box between
    /// `a` and `b`, ignoring every edit; nothing is loaded or changed.
    pub fn generated_blocks(&self, a: BlockPos, b: BlockPos) -> Result<BlockGrid, WorldError> {
        self.generated_blocks_with(a, b, None, None)
    }

    /// Like [`World::generated_blocks`], generated with `seed` and one
    /// `biome` everywhere in place of the world's where given.
    pub fn generated_blocks_with(
        &self,
        a: BlockPos,
        b: BlockPos,
        seed: Option<i64>,
        biome: Option<&str>,
    ) -> Result<BlockGrid, WorldError> {
        host::generated_blocks_get(&self.key, a, b, seed, biome).map(|found| BlockGrid {
            found,
            a,
            b,
        })
    }

    /// Sets the block at `at`, updating its neighbours.
    pub fn set_block(&self, at: BlockPos, state: &str) -> Result<(), WorldError> {
        host::block_set(&self.key, at, state, SetFlags::empty())
    }

    /// Sets the block at `at` without updating its neighbours.
    pub fn set_block_quietly(&self, at: BlockPos, state: &str) -> Result<(), WorldError> {
        host::block_set(&self.key, at, state, SetFlags::NO_UPDATES)
    }

    /// The biome name of the 4x4x4 cell holding `at`; loads the chunk.
    pub fn biome(&self, at: BlockPos) -> Result<String, WorldError> {
        host::biome_get(&self.key, at)
    }

    /// Queues setting the biome of every cell holding a block of the box
    /// between `a` and `b`; cells in unloaded chunks are skipped.
    pub fn set_biome(&self, a: BlockPos, b: BlockPos, biome: &str) -> Result<(), WorldError> {
        host::biome_set(&self.key, a, b, biome)
    }

    /// The loaded non-player entities inside the box from `a` to `b`.
    pub fn entities(&self, a: Vec3, b: Vec3) -> Result<Vec<Entity>, WorldError> {
        host::entities_in_box(&self.key, a, b)
    }

    /// Spawns an entity of type `kind` (such as `minecraft:pig`), with
    /// optional SNBT data.
    pub fn spawn(&self, kind: &str, at: Vec3, snbt: Option<&str>) -> Result<EntityRef, WorldError> {
        host::spawn_entity(&self.key, kind, at, snbt).map(EntityRef)
    }

    /// Shows a particle effect to the players near it.
    pub fn particles(&self, particle: &Particle) -> Result<(), WorldError> {
        host::spawn_particles(&self.key, &particle.0, None)
    }

    /// Shows a particle effect to one player, if they are near it.
    pub fn particles_for(&self, viewer: Uuid, particle: &Particle) -> Result<(), WorldError> {
        host::spawn_particles(&self.key, &particle.0, Some(viewer))
    }
}

/// A particle effect with typed options, as `/particle` shows it:
/// `Particle::dust(0xff0000, 1.0, at).spread(delta, 0.0, 20)`.
#[derive(Clone, Debug)]
pub struct Particle(ParticleSpawn);

impl Particle {
    /// A particle that takes no options, such as `minecraft:flame`, or one
    /// whose options are set with [`Particle::options`].
    pub fn new(particle: &str, at: Vec3) -> Self {
        Self(ParticleSpawn {
            particle: particle.to_owned(),
            options: ParticleOptions::None,
            pos: at,
            delta: Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            speed: 0.0,
            count: 0,
            force: false,
        })
    }

    pub fn options(mut self, options: ParticleOptions) -> Self {
        self.0.options = options;
        self
    }

    /// `minecraft:dust` in an RGB colour (`0xRRGGBB`), size 0.01 to 4.
    pub fn dust(rgb: u32, scale: f32, at: Vec3) -> Self {
        Self::new("minecraft:dust", at)
            .options(ParticleOptions::Dust(DustOptions { color: rgb, scale }))
    }

    /// `minecraft:dust_color_transition` from one RGB colour to another.
    pub fn dust_transition(from: u32, to: u32, scale: f32, at: Vec3) -> Self {
        Self::new("minecraft:dust_color_transition", at).options(ParticleOptions::DustTransition(
            DustTransitionOptions { from, to, scale },
        ))
    }

    /// A block particle (`minecraft:block`, `block_marker`,
    /// `falling_dust`, `dust_pillar`, `block_crumble`) of a block state.
    pub fn block(particle: &str, state: &str, at: Vec3) -> Self {
        Self::new(particle, at).options(ParticleOptions::Block(state.to_owned()))
    }

    /// `minecraft:item` showing one of `item`.
    pub fn item(item: &str, at: Vec3) -> Self {
        Self::new("minecraft:item", at).options(ParticleOptions::Item(item.to_owned()))
    }

    /// `count` particles spread by `delta` at `speed`; count 0 sends one
    /// moving along `delta`.
    pub fn spread(mut self, delta: Vec3, speed: f32, count: i32) -> Self {
        self.0.delta = delta;
        self.0.speed = speed;
        self.0.count = count;
        self
    }

    /// Seen from 512 blocks instead of 32.
    pub fn force(mut self) -> Self {
        self.0.force = true;
        self
    }

    pub fn spawn(&self) -> &ParticleSpawn {
        &self.0
    }
}

/// A loaded non-player entity, by uuid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EntityRef(pub Uuid);

impl EntityRef {
    pub fn uuid(self) -> Uuid {
        self.0
    }

    pub fn remove(self) -> Result<(), WorldError> {
        host::remove_entity(self.0)
    }

    /// The entity's data as SNBT.
    pub fn data(self) -> Result<Option<String>, WorldError> {
        host::entity_data(self.0)
    }

    /// Merges SNBT `snbt` into the entity's data.
    pub fn merge(self, snbt: &str) -> Result<(), WorldError> {
        host::entity_merge(self.0, snbt)
    }

    pub fn rename(self, name: Option<&str>) -> Result<(), WorldError> {
        host::entity_set_name(self.0, name)
    }

    /// Swings this entity's main hand at `victim` as a player's melee
    /// attack would; skipped when `victim` is beyond 3 blocks.
    pub fn strike(self, victim: EntityRef) -> Result<(), WorldError> {
        host::entity_strike(self.0, victim.0)
    }

    pub fn teleport(self, world: &World, to: Vec3) -> Result<(), WorldError> {
        host::teleport_entity(self.0, &world.key, to, None)
    }
}

impl From<&Entity> for EntityRef {
    fn from(entity: &Entity) -> Self {
        EntityRef(entity.uuid)
    }
}

/// Calls on any online player.
pub trait PlayerExt {
    /// Messages the player from any callback.
    fn tell(&self, text: impl AsRef<str>) -> Result<(), crate::HostError>;

    /// Messages the player with click and hover events; see [`Rich`].
    fn tell_rich(&self, message: &crate::text::Rich) -> Result<(), crate::HostError>;

    /// A [`Replier`] for the player, to answer after an `await`.
    fn replier(&self) -> Replier;

    fn teleport(
        &self,
        world: &World,
        to: Vec3,
        rotation: Option<Rotation>,
    ) -> Result<(), WorldError>;

    /// Shows a title with default timings (10, 70, 20 ticks); see [`Title`].
    fn title(&self, title: impl Into<Span>) -> Result<(), crate::HostError>;

    fn show_title(&self, title: &Title) -> Result<(), crate::HostError>;

    fn clear_title(&self) -> Result<(), crate::HostError>;

    fn action_bar(&self, text: impl Into<Span>) -> Result<(), crate::HostError>;

    /// Meets `criterion` (every one when `None`) of a `cap.content` advancement.
    fn grant_advancement(
        &self,
        advancement: &str,
        criterion: Option<&str>,
    ) -> Result<(), WorldError>;

    fn revoke_advancement(
        &self,
        advancement: &str,
        criterion: Option<&str>,
    ) -> Result<(), WorldError>;

    /// Gives `count` of `item`; what does not fit drops at their feet.
    fn give(&self, item: &str, count: u32) -> Result<(), WorldError>;

    /// Removes up to `count` of `item`; all or nothing when `all_or_nothing`.
    /// Edits from one call apply in call order after it returns, and a
    /// failed all-or-nothing take skips the later ones, so `take` then
    /// `give` is an exchange.
    fn take(&self, item: &str, count: u32, all_or_nothing: bool) -> Result<(), WorldError>;

    fn clear_inventory(&self) -> Result<(), WorldError>;

    /// Replaces a slot with SNBT `{id, count, components}`; `None` empties it.
    fn set_slot(&self, slot: u8, stack: Option<&str>) -> Result<(), WorldError>;

    /// Overrides keep-inventory for them; `None` follows the rule again.
    fn set_keep_inventory(&self, keep: Option<bool>) -> Result<(), WorldError>;
}

/// A title and subtitle with fade timings in ticks.
#[derive(Clone, Debug, PartialEq)]
pub struct Title {
    pub title: Span,
    pub subtitle: Span,
    pub fade_in: u32,
    pub stay: u32,
    pub fade_out: u32,
}

impl Title {
    pub fn new(title: impl Into<Span>) -> Self {
        Title {
            title: title.into(),
            subtitle: Span::new(""),
            fade_in: 10,
            stay: 70,
            fade_out: 20,
        }
    }

    pub fn subtitle(mut self, subtitle: impl Into<Span>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    pub fn times(mut self, fade_in: u32, stay: u32, fade_out: u32) -> Self {
        self.fade_in = fade_in;
        self.stay = stay;
        self.fade_out = fade_out;
        self
    }
}

/// This plugin's sidebar, shown to everyone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sidebar {
    title: Span,
    lines: Vec<Span>,
}

impl Sidebar {
    pub fn new(title: impl Into<Span>) -> Self {
        Sidebar {
            title: title.into(),
            lines: Vec::new(),
        }
    }

    /// Appends a line below the previous ones (the host keeps up to 15).
    pub fn line(mut self, line: impl Into<Span>) -> Self {
        self.lines.push(line.into());
        self
    }

    pub fn lines(mut self, lines: impl IntoIterator<Item = Span>) -> Self {
        self.lines.extend(lines);
        self
    }

    /// Shows it, replacing the previous one.
    pub fn show(&self) -> Result<(), crate::HostError> {
        let lines: Vec<String> = self.lines.iter().map(Span::to_string).collect();
        host::sidebar_set(&self.title.to_string(), &lines).map_err(Into::into)
    }

    pub fn clear() -> Result<(), crate::HostError> {
        host::sidebar_clear().map_err(Into::into)
    }
}

/// A scoreboard team owned by this plugin.
#[derive(Clone, Debug, PartialEq)]
pub struct Team {
    name: String,
    color: Option<String>,
    prefix: Span,
    suffix: Span,
    members: Vec<String>,
}

impl Team {
    pub fn new(name: impl Into<String>) -> Self {
        Team {
            name: name.into(),
            color: None,
            prefix: Span::new(""),
            suffix: Span::new(""),
            members: Vec::new(),
        }
    }

    /// A chat color name such as `red`.
    pub fn color(mut self, color: impl Into<String>) -> Self {
        self.color = Some(color.into());
        self
    }

    pub fn prefix(mut self, prefix: impl Into<Span>) -> Self {
        self.prefix = prefix.into();
        self
    }

    pub fn suffix(mut self, suffix: impl Into<Span>) -> Self {
        self.suffix = suffix.into();
        self
    }

    pub fn member(mut self, name: impl Into<String>) -> Self {
        self.members.push(name.into());
        self
    }

    pub fn members<S: Into<String>>(mut self, names: impl IntoIterator<Item = S>) -> Self {
        self.members.extend(names.into_iter().map(Into::into));
        self
    }

    /// Creates or updates the team with exactly these members.
    pub fn apply(&self) -> Result<(), crate::HostError> {
        host::team_set(
            &self.name,
            &crate::types::TeamSpec {
                color: self.color.clone(),
                prefix: self.prefix.to_string(),
                suffix: self.suffix.to_string(),
                members: self.members.clone(),
            },
        )
        .map_err(Into::into)
    }

    pub fn remove(name: &str) -> Result<(), crate::HostError> {
        host::team_remove(name).map_err(Into::into)
    }
}

/// Declares permission `node` (plugin id, a dot, a name); level 0 is for
/// everyone, 1 to 4 for operators. Only in `Plugin::register`.
pub fn declare_permission(node: &str, level: u8) -> bool {
    host::declare_permission(node, level).is_ok()
}

impl PlayerExt for Player {
    fn tell(&self, text: impl AsRef<str>) -> Result<(), crate::HostError> {
        host::tell(self.uuid, text.as_ref()).map_err(Into::into)
    }

    fn tell_rich(&self, message: &crate::text::Rich) -> Result<(), crate::HostError> {
        host::tell_rich(self.uuid, message.spans()).map_err(Into::into)
    }

    fn replier(&self) -> Replier {
        Replier::to(Some(self.uuid))
    }

    fn teleport(
        &self,
        world: &World,
        to: Vec3,
        rotation: Option<Rotation>,
    ) -> Result<(), WorldError> {
        host::teleport_player(self.uuid, &world.key, to, rotation)
    }

    fn title(&self, title: impl Into<Span>) -> Result<(), crate::HostError> {
        self.show_title(&Title::new(title))
    }

    fn show_title(&self, t: &Title) -> Result<(), crate::HostError> {
        host::show_title(
            self.uuid,
            &crate::types::TitleSpec {
                title: t.title.to_string(),
                subtitle: t.subtitle.to_string(),
                fade_in: t.fade_in,
                stay: t.stay,
                fade_out: t.fade_out,
            },
        )
        .map_err(Into::into)
    }

    fn clear_title(&self) -> Result<(), crate::HostError> {
        host::clear_title(self.uuid).map_err(Into::into)
    }

    fn action_bar(&self, text: impl Into<Span>) -> Result<(), crate::HostError> {
        host::action_bar(self.uuid, &text.into().to_string()).map_err(Into::into)
    }

    fn grant_advancement(
        &self,
        advancement: &str,
        criterion: Option<&str>,
    ) -> Result<(), WorldError> {
        host::advancement_grant(self.uuid, advancement, criterion)
    }

    fn revoke_advancement(
        &self,
        advancement: &str,
        criterion: Option<&str>,
    ) -> Result<(), WorldError> {
        host::advancement_revoke(self.uuid, advancement, criterion)
    }

    fn give(&self, item: &str, count: u32) -> Result<(), WorldError> {
        host::give_item_to(self.uuid, item, count, None, None)
    }

    fn take(&self, item: &str, count: u32, all_or_nothing: bool) -> Result<(), WorldError> {
        host::take_items(self.uuid, item, count, all_or_nothing)
    }

    fn clear_inventory(&self) -> Result<(), WorldError> {
        host::clear_inventory(self.uuid)
    }

    fn set_slot(&self, slot: u8, stack: Option<&str>) -> Result<(), WorldError> {
        host::set_slot(self.uuid, slot, stack)
    }

    fn set_keep_inventory(&self, keep: Option<bool>) -> Result<(), WorldError> {
        host::set_keep_inventory(self.uuid, keep)
    }
}

/// The player the current callback acts for (a command's sender, an
/// event's player).
pub mod acting {
    use super::*;
    use crate::types::{Inventory, Location};

    pub fn player() -> Option<Player> {
        host::acting_player()
    }

    pub fn location() -> Option<Location> {
        host::acting_location()
    }

    /// The player's dimension, from their location.
    pub fn world() -> Option<World> {
        location().map(|l| World::new(l.dimension))
    }

    /// Eye position and look of the acting player, when the call knows it.
    pub fn eye() -> Option<(Vec3, Rotation)> {
        let at = host::acting_location()?;
        let look = at.rotation?;
        Some((
            Vec3 {
                y: at.pos.y + at.eye_height,
                ..at.pos
            },
            look,
        ))
    }

    pub fn inventory() -> Option<Inventory> {
        host::acting_inventory()
    }

    /// Their occupied slots as `(index, stack SNBT)`.
    pub fn inventory_snbt() -> Option<Vec<(u8, String)>> {
        host::acting_inventory_snbt()
            .map(|slots| slots.into_iter().map(|s| (s.index, s.stack)).collect())
    }

    /// Whether they hold permission `node`; `None` when no player acts.
    pub fn has_permission(node: &str) -> Option<bool> {
        host::has_permission(node)
    }

    /// Gives them `count` of `item`; what does not fit drops at their feet.
    pub fn give(item: &str, count: u32) -> Result<(), WorldError> {
        host::give_item(item, count, None, None)
    }

    /// Like [`give`], with `custom_data` (SNBT of a compound) and a custom name.
    pub fn give_tagged(
        item: &str,
        count: u32,
        custom_data: Option<&str>,
        custom_name: Option<&str>,
    ) -> Result<(), WorldError> {
        host::give_item(item, count, custom_data, custom_name)
    }

    /// Moves them within their dimension.
    pub fn teleport(to: Vec3, rotation: Option<Rotation>) -> Result<(), WorldError> {
        host::teleport_self(to, rotation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn title_builder_sets_times_and_subtitle() {
        let t = Title::new("a").subtitle("b").times(1, 2, 3);
        assert_eq!((t.fade_in, t.stay, t.fade_out), (1, 2, 3));
        assert_eq!(t.subtitle.to_string(), "b");
    }

    #[tuff_core::watchdog]
    #[test]
    fn grid_resolves_palette_and_outside() {
        let grid = BlockGrid {
            found: BlockBox {
                palette: vec!["a".into(), "b".into()],
                indices: vec![1, 0, 65535],
            },
            a: pos(0, 0, 0),
            b: pos(2, 0, 0),
        };
        let all: Vec<_> = grid.iter().collect();
        assert_eq!(all, vec![Some("b"), Some("a"), None]);
        assert_eq!(grid.get(9), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn transform_maps_to_wit() {
        assert_eq!(
            Transform::from(BlockTransform::Rotate180),
            Transform::Rotate180
        );
        assert_eq!(BlockState::from("x").to_string(), "x");
    }
}
