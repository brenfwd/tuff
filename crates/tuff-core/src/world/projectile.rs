//! What projectiles share across features: who launched one, the math of
//! a shot, hit tests along a flight path, and [`Launch`], the description
//! of a new projectile that the feature implementing its type builds
//! through [`crate::world::entity::Registry::launch`].

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, Item};

use crate::world::effects::ActiveEffect;
use crate::world::entity::{Culprit, Effect, EntityBase, TickContext, take, take_bool};
use crate::world::messages::PlayerHandle;
use crate::world::physics::{self, Aabb};
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

/// How far a projectile looks around its path for entities to hit.
const SEARCH_INFLATE: f64 = 1.0;

/// Who shot or threw a projectile.
#[derive(Debug, Clone, Default)]
pub struct Owner {
    pub uuid: Option<u128>,
    /// The owner as it was when it shot, while this server runs.
    pub culprit: Option<Culprit>,
    /// A player owner's session, while this server runs.
    pub handle: Option<PlayerHandle>,
    /// It has moved clear of its owner, which it may now hit.
    pub left: bool,
    pub has_been_shot: bool,
}

impl Owner {
    pub fn of(culprit: Culprit, handle: Option<PlayerHandle>) -> Self {
        Self {
            uuid: Some(culprit.uuid),
            culprit: Some(culprit),
            handle,
            left: false,
            has_been_shot: false,
        }
    }

    /// The owner's current entity id and box, if the region sees it.
    pub fn find(&self, cx: &TickContext) -> Option<(i32, Aabb)> {
        let uuid = self.uuid?;
        if let Some(player) = cx.players.iter().find(|p| p.uuid == uuid) {
            return Some((player.id, player.bounds()));
        }
        cx.entities
            .iter()
            .find(|e| e.uuid == uuid)
            .map(|e| (e.id, e.bounds))
    }

    /// The owner to blame for a hit: as it shot, or as the region sees it
    /// after a reload.
    pub fn culprit(&self, cx: &TickContext) -> Option<Culprit> {
        if let Some(culprit) = &self.culprit {
            return Some(culprit.clone());
        }
        let uuid = self.uuid?;
        if let Some(player) = cx.players.iter().find(|p| p.uuid == uuid) {
            return Some(Culprit::player(player.id, uuid, player.name));
        }
        let entity = cx.entities.iter().find(|e| e.uuid == uuid)?;
        let base = EntityBase::new(entity.kind, entity.id, uuid, entity.position, [0.0; 3]);
        Some(Culprit::entity(&base))
    }

    pub fn is_player(&self, cx: &TickContext) -> bool {
        match &self.culprit {
            Some(culprit) => culprit.is_player(),
            None => self
                .uuid
                .is_some_and(|uuid| cx.players.iter().any(|p| p.uuid == uuid)),
        }
    }

    /// Once no longer touching its owner, a projectile may hit it.
    pub fn check_left(&mut self, bounds: Aabb, velocity: [f64; 3], cx: &TickContext) {
        if self.left {
            return;
        }
        let Some((_, owner)) = self.find(cx) else {
            self.left = true;
            return;
        };
        let area = physics::inflate(physics::expand_towards(bounds, velocity), [1.0; 3]);
        self.left = !physics::intersects(area, owner);
    }

    pub fn load(&mut self, nbt: &mut Compound) {
        self.uuid = take(nbt, "Owner", Tag::as_uuid);
        self.left = take_bool(nbt, "LeftOwner").unwrap_or(false);
        self.has_been_shot = take_bool(nbt, "HasBeenShot").unwrap_or(false);
    }

    pub fn save(&self, out: &mut Compound) {
        if let Some(uuid) = self.uuid {
            out.insert("Owner", Tag::uuid(uuid));
        }
        if self.left {
            out.insert("LeftOwner", true);
        }
        out.insert("HasBeenShot", self.has_been_shot);
    }

    /// The owner's entity id for the spawn packet (0 when unknown).
    pub fn spawn_data(&self) -> i32 {
        self.culprit.as_ref().map_or(0, |c| c.id)
    }
}

pub use crate::world::physics::triangle;

/// The velocity of a shot in direction `(x, y, z)` at `power`, spread by
/// `uncertainty`.
pub fn shot_velocity(
    direction: [f64; 3],
    power: f32,
    uncertainty: f32,
    rng: &mut StdRng,
) -> [f64; 3] {
    let length = direction.iter().map(|c| c * c).sum::<f64>().sqrt();
    let unit = if length < 1.0e-4 {
        [0.0; 3]
    } else {
        direction.map(|c| c / length)
    };
    let spread = 0.017_227_5 * f64::from(uncertainty);
    let mut velocity = [0.0; 3];
    for axis in 0..3 {
        velocity[axis] = (unit[axis] + triangle(rng, 0.0, spread)) * f64::from(power);
    }
    velocity
}

/// A shot along a yaw and pitch (degrees), as players shoot and throw.
pub fn shot_from_rotation(
    pitch: f32,
    yaw: f32,
    pitch_offset: f32,
    power: f32,
    uncertainty: f32,
    rng: &mut StdRng,
) -> [f64; 3] {
    let (pitch_r, yaw_r) = (pitch.to_radians(), yaw.to_radians());
    let x = -yaw_r.sin() * pitch_r.cos();
    let y = -(pitch + pitch_offset).to_radians().sin();
    let z = yaw_r.cos() * pitch_r.cos();
    shot_velocity(
        [f64::from(x), f64::from(y), f64::from(z)],
        power,
        uncertainty,
        rng,
    )
}

/// The yaw and pitch a projectile faces when moving along `velocity`.
pub fn rotation_of(velocity: [f64; 3]) -> (f32, f32) {
    let horizontal = (velocity[0] * velocity[0] + velocity[2] * velocity[2]).sqrt();
    let yaw = velocity[0].atan2(velocity[2]).to_degrees() as f32;
    let pitch = velocity[1].atan2(horizontal).to_degrees() as f32;
    (yaw, pitch)
}

/// Turns `current` a fifth of the way to `target`, the short way round.
pub fn lerp_rotation(current: f32, target: f32) -> f32 {
    let mut current = current;
    while target - current < -180.0 {
        current -= 360.0;
    }
    while target - current >= 180.0 {
        current += 360.0;
    }
    current + 0.2 * (target - current)
}

/// Where the segment `from`..`to` enters `bounds`, from outside.
pub fn clip(bounds: Aabb, from: [f64; 3], to: [f64; 3]) -> Option<[f64; 3]> {
    let mut enter = 0.0f64;
    let mut exit = 1.0f64;
    let mut entered_at_start = true;
    for axis in 0..3 {
        let delta = to[axis] - from[axis];
        let (min, max) = (bounds[axis], bounds[axis + 3]);
        if delta.abs() < 1.0e-12 {
            if from[axis] < min || from[axis] > max {
                return None;
            }
            continue;
        }
        let (mut t0, mut t1) = ((min - from[axis]) / delta, (max - from[axis]) / delta);
        if t0 > t1 {
            std::mem::swap(&mut t0, &mut t1);
        }
        if t0 > enter {
            enter = t0;
            entered_at_start = false;
        }
        exit = exit.min(t1);
        if enter > exit {
            return None;
        }
    }
    if entered_at_start {
        return None;
    }
    Some([0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * enter))
}

/// The margin a projectile's hit test grows boxes by: none on its first
/// ticks, up to 0.3 later.
pub fn margin(ticks: u32) -> f64 {
    ((f64::from(ticks) - 2.0) / 20.0).clamp(0.0, 0.3)
}

/// A living target a projectile's path crosses.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub id: i32,
    pub kind: EntityType,
    pub position: [f64; 3],
    pub bounds: Aabb,
    /// Where the path meets it.
    pub location: [f64; 3],
}

fn is_fireball(kind: EntityType) -> bool {
    matches!(kind, EntityType::Fireball | EntityType::SmallFireball)
}

/// Players and living entities (other than `skip`) whose boxes, grown by
/// `margin`, the segment `from`..`to` enters.
pub fn targets_along(
    projectile: &EntityBase,
    bounds: Aabb,
    from: [f64; 3],
    to: [f64; 3],
    margin: f64,
    skip: impl Fn(i32) -> bool,
    cx: &TickContext,
) -> Vec<Target> {
    let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let area = physics::inflate(physics::expand_towards(bounds, delta), [SEARCH_INFLATE; 3]);
    let mut found = Vec::new();
    let mut consider = |id: i32, kind: EntityType, position: [f64; 3], target: Aabb| {
        if id == projectile.id
            || skip(id)
            || (is_fireball(projectile.kind) && is_fireball(kind))
            || !physics::intersects(area, target)
        {
            return;
        }
        let grown = physics::inflate(target, [margin; 3]);
        if let Some(location) = clip(grown, from, to) {
            found.push(Target {
                id,
                kind,
                position,
                bounds: target,
                location,
            });
        }
    };
    for player in cx.players {
        consider(
            player.id,
            EntityType::Player,
            player.position,
            player.bounds(),
        );
    }
    for entity in cx.entities {
        if entity.pickable || entity.living.is_some_and(|l| l.alive) {
            consider(entity.id, entity.kind, entity.position, entity.bounds);
        }
    }
    for part in cx.parts {
        if !skip(part.owner) {
            consider(part.id, part.kind, part.position, part.bounds);
        }
    }
    found.sort_by(|a, b| {
        crate::world::distance_sq(from, a.location)
            .total_cmp(&crate::world::distance_sq(from, b.location))
    });
    found
}

/// Whether `target` turns `projectile` away instead of being hit by it:
/// the entity types of the `deflects_projectiles` tag (the breeze) do this
/// to everything but wind charges. A projectile flying towards it is sent
/// back at half speed with a sound; one already leaving it still passes
/// through unharmed.
pub fn deflect_off(projectile: &mut EntityBase, target: &Target, cx: &mut TickContext) -> bool {
    if matches!(
        projectile.kind,
        EntityType::WindCharge | EntityType::BreezeWindCharge
    ) || !tuff_datapack::tags::is(
        tuff_vanilla_data::tag::entity_type::key::DEFLECTS_PROJECTILES,
        target.kind,
    ) {
        return false;
    }
    let to = [
        target.position[0] - projectile.position[0],
        target.position[1] - projectile.position[1],
        target.position[2] - projectile.position[2],
    ];
    let v = projectile.velocity;
    if v[0] * to[0] + v[1] * to[1] + v[2] * to[2] <= 0.0 {
        return true;
    }
    projectile.velocity = v.map(|c| -c * 0.5);
    projectile.yaw += 170.0 + cx.rng.r#gen::<f32>() * 20.0;
    let packet = crate::world::framed(&crate::protocol::play::Sound {
        sound_event: SoundEvent::EntityBreezeDeflect.id(),
        category: crate::protocol::play::sound_category::HOSTILE,
        position: target.position,
        volume: 1.0,
        pitch: 1.0,
        seed: cx.rng.r#gen(),
    });
    cx.effects.push(crate::world::entity::Effect::ChunkPackets(
        crate::world::packets([packet]),
    ));
    true
}

/// The first block whose collision shape the segment hits; unknown blocks
/// count as solid.
pub fn block_hit(
    from: [f64; 3],
    to: [f64; 3],
    cx: &TickContext,
) -> Option<crate::world::raycast::Hit> {
    let level = |pos: BlockPos| (cx.block)(pos).unwrap_or(Block::Stone.default_state());
    crate::world::raycast::cast_colliders(from, to, &level)
}

/// Where a segment that starts inside the world border leaves it, clamped
/// to the border, if the segment ends outside.
pub fn border_stop(from: [f64; 3], to: [f64; 3], cx: &TickContext) -> Option<[f64; 3]> {
    let border = cx.shared.dimension(cx.dimension).border();
    if !border.contains(from[0], from[2]) || border.contains(to[0], to[2]) {
        return None;
    }
    let [x, z] = border.clamp(to[0], to[2]);
    Some([x, to[1], z])
}

/// What a projectile that bounces off the world border does on reaching
/// it: it turns around at a fifth of its speed, halved.
pub fn bounce_off_border(base: &mut EntityBase, rng: &mut StdRng) {
    base.velocity = base.velocity.map(|c| c * -0.2 * 0.5);
    base.yaw += 170.0 + rng.r#gen::<f32>() * 20.0;
}

/// Tells the block a projectile struck about it.
pub fn struck_block(
    hit: crate::world::raycast::Hit,
    base: &EntityBase,
    owner: &Owner,
    cx: &mut TickContext,
) {
    let impact = crate::world::entity::ProjectileImpact {
        kind: base.kind,
        arrow: matches!(
            base.kind,
            EntityType::Arrow | EntityType::SpectralArrow | EntityType::Trident
        ),
        on_fire: base.on_fire(),
        velocity: base.velocity,
        owner: owner.culprit(cx),
    };
    let owner = impact.owner.clone();
    let pos = hit.pos;
    cx.effects
        .push(crate::world::entity::Effect::ProjectileHitBlock {
            hit,
            impact: Box::new(impact),
        });
    landed(base, owner, Some(pos), None, cx);
}

/// Reports a projectile landing on `block` or on the entity `target`.
pub fn landed(
    base: &EntityBase,
    owner: Option<Culprit>,
    block: Option<BlockPos>,
    target: Option<i32>,
    cx: &mut TickContext,
) {
    cx.effects
        .push(crate::world::entity::Effect::ProjectileLanded(Box::new(
            crate::world::entity::ProjectileLanding {
                id: base.id,
                uuid: base.uuid,
                kind: base.kind,
                position: base.position,
                owner,
                block,
                target,
            },
        )));
}

/// Whether it is thundering where the projectile flies.
pub fn thundering(cx: &TickContext) -> bool {
    let kind = cx.shared.dimension(cx.dimension).kind;
    kind.has_skylight && !kind.has_ceiling && cx.shared.weather.borrow().is_thundering()
}

/// Whether the sky shows at `pos`.
pub fn sees_sky(pos: BlockPos, cx: &TickContext) -> bool {
    (cx.light)(pos).is_some_and(|(sky, _)| sky >= 15)
}

/// Channeling: a lightning bolt at `position`, with the trident's thunder.
pub fn call_lightning(position: [f64; 3], cx: &mut TickContext) {
    if let Some(bolt) = cx
        .shared
        .entity_types
        .create(EntityType::LightningBolt, 0, position, 0.0)
    {
        cx.effects.push(crate::world::entity::Effect::Spawn(bolt));
    }
    let packet = crate::world::framed(&crate::protocol::play::Sound {
        sound_event: tuff_vanilla_data::registry::SoundEvent::ItemTridentThunder.id(),
        category: crate::protocol::play::sound_category::NEUTRAL,
        position,
        volume: 5.0,
        pitch: 1.0,
        seed: rand::Rng::r#gen(cx.rng),
    });
    cx.effects.push(crate::world::entity::Effect::ChunkPackets(
        crate::world::packets([packet]),
    ));
}

/// Whether a box is in water, as an entity's water state is updated.
pub fn in_water(bounds: Aabb, cx: &TickContext) -> bool {
    let eye = [bounds[0], bounds[1], bounds[2]];
    physics::fluid_heights(bounds, eye, cx.block).water > 0.0
}

/// The knockback direction a projectile gives what it hits: along its own
/// flight, expressed as a source position behind the victim.
pub fn hit_source(victim: [f64; 3], velocity: [f64; 3]) -> [f64; 3] {
    [victim[0] - velocity[0], victim[1], victim[2] - velocity[2]]
}
/// Who may pick an arrow up once it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pickup {
    Disallowed,
    Allowed,
    CreativeOnly,
}

impl Pickup {
    pub fn from_byte(value: i8) -> Self {
        match value {
            1 => Self::Allowed,
            2 => Self::CreativeOnly,
            _ => Self::Disallowed,
        }
    }

    pub fn byte(self) -> i8 {
        match self {
            Self::Disallowed => 0,
            Self::Allowed => 1,
            Self::CreativeOnly => 2,
        }
    }
}

/// The entity a thrown item becomes.
pub fn thrown_kind(item: Item) -> Option<EntityType> {
    Some(match item {
        Item::Snowball => EntityType::Snowball,
        Item::Egg | Item::BlueEgg | Item::BrownEgg => EntityType::Egg,
        Item::EnderPearl => EntityType::EnderPearl,
        Item::SplashPotion => EntityType::SplashPotion,
        Item::LingeringPotion => EntityType::LingeringPotion,
        Item::ExperienceBottle => EntityType::ExperienceBottle,
        _ => return None,
    })
}

/// A rocket item's flight duration and stars; a rocket with no
/// `fireworks` component flies for 1.
pub fn firework_flight(stack: &ItemStack) -> (u8, usize) {
    match &stack.components.fireworks {
        Some(fireworks) => (fireworks.flight_duration, fireworks.explosions.len()),
        None if stack.item() == Item::FireworkRocket => (1, 0),
        None => (0, 0),
    }
}

/// How many ticks a rocket flies before it explodes.
pub fn firework_lifetime(stack: &ItemStack, rng: &mut impl Rng) -> i32 {
    let (duration, _) = firework_flight(stack);
    10 * (1 + i32::from(duration)) + rng.gen_range(0..6) + rng.gen_range(0..7)
}

/// An arrow, spectral arrow or trident about to be shot.
#[derive(Debug)]
pub struct ArrowShot {
    /// The arrow or trident item; a pickup gives it back.
    pub item: ItemStack,
    pub velocity: [f64; 3],
    /// Who may pick it up; `None` lets players pick up their own.
    pub pickup: Option<Pickup>,
    pub crit: bool,
    /// Damage before speed scaling; `None` keeps the type's own.
    pub damage: Option<f64>,
    pub pierce: u8,
    pub fire_ticks: i16,
    /// The bow or crossbow it left; its enchantments act on the hit.
    pub weapon: Option<tuff_item::item::ItemSnapshot>,
    /// The sound it lands with; `None` keeps the type's own.
    pub sound: Option<SoundEvent>,
    /// Effects it gives what it hits besides its item's potion.
    pub effects: Vec<ActiveEffect>,
}

impl ArrowShot {
    pub fn new(item: ItemStack, velocity: [f64; 3]) -> Self {
        Self {
            item,
            velocity,
            pickup: None,
            crit: false,
            damage: None,
            pierce: 0,
            fire_ticks: 0,
            weapon: None,
            sound: None,
            effects: Vec::new(),
        }
    }

    /// The entity its item flies as: tridents and spectral arrows are
    /// their own types; plain and tipped arrows are arrows.
    pub fn kind(&self) -> EntityType {
        match self.item.item() {
            Item::Trident => EntityType::Trident,
            Item::SpectralArrow => EntityType::SpectralArrow,
            _ => EntityType::Arrow,
        }
    }
}

/// How a firework rocket leaves.
#[derive(Debug, Clone, Copy)]
pub enum Flight {
    /// Shot along a velocity, as from a crossbow.
    Shot([f64; 3]),
    /// Launched upward with a little drift.
    Launched,
    /// Boosting the gliding player with this entity id for `lifetime` ticks.
    Attached { player: i32, lifetime: i32 },
}

/// What kind of projectile a [`Launch`] makes, with what only it needs.
#[derive(Debug)]
pub enum Shot {
    Arrow(ArrowShot),
    /// A thrown item: snowball, egg, ender pearl, potion or experience
    /// bottle, as [`thrown_kind`] maps it.
    Thrown {
        kind: EntityType,
        item: ItemStack,
        velocity: [f64; 3],
    },
    Firework {
        item: ItemStack,
        flight: Flight,
    },
    /// A player's (`WindCharge`) or a breeze's (`BreezeWindCharge`).
    WindCharge {
        kind: EntityType,
        velocity: [f64; 3],
    },
    SmallFireball {
        direction: [f64; 3],
    },
    LargeFireball {
        direction: [f64; 3],
        explosion_power: i32,
    },
    WitherSkull {
        direction: [f64; 3],
        dangerous: bool,
    },
    EyeOfEnder {
        item: ItemStack,
        target: BlockPos,
    },
    /// A fishing bobber cast from a rod along a yaw and pitch.
    Bobber {
        rotation: (f32, f32),
        rod: tuff_item::item::ItemSnapshot,
        luck: f32,
    },
}

impl Shot {
    /// The entity type it makes, which picks the feature that builds it.
    pub fn kind(&self) -> EntityType {
        match self {
            Self::Arrow(arrow) => arrow.kind(),
            Self::Thrown { kind, .. } | Self::WindCharge { kind, .. } => *kind,
            Self::Firework { .. } => EntityType::FireworkRocket,
            Self::SmallFireball { .. } => EntityType::SmallFireball,
            Self::LargeFireball { .. } => EntityType::Fireball,
            Self::WitherSkull { .. } => EntityType::WitherSkull,
            Self::EyeOfEnder { .. } => EntityType::EyeOfEnder,
            Self::Bobber { .. } => EntityType::FishingBobber,
        }
    }
}

/// A new projectile: its entity id, where it starts, who launched it and
/// what it is.
#[derive(Debug)]
pub struct Launch {
    pub id: i32,
    pub position: [f64; 3],
    pub owner: Owner,
    pub shot: Shot,
}

impl Launch {
    pub fn kind(&self) -> EntityType {
        self.shot.kind()
    }
}

/// Builds the entity of a [`Launch`]; registered per type by the feature
/// that implements it.
pub type Launcher = fn(Launch, &mut StdRng) -> Option<Box<dyn crate::world::entity::RegionEntity>>;

/// Spawns `launch` from a ticking entity; false if no feature implements
/// its type.
pub fn launch(launch: Launch, cx: &mut TickContext) -> bool {
    let Some(entity) = cx.shared.entity_types.launch(launch, cx.rng) else {
        return false;
    };
    cx.effects.push(Effect::Spawn(entity));
    true
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use tuff_item::item::MintReason;

    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_launch_no_feature_implements_makes_nothing() {
        let registry = crate::world::entity::Registry::default();
        let launch = Launch {
            id: 1,
            position: [0.0; 3],
            owner: Owner::default(),
            shot: Shot::SmallFireball {
                direction: [1.0, 0.0, 0.0],
            },
        };
        assert!(
            registry
                .launch(launch, &mut StdRng::seed_from_u64(0))
                .is_none()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn arrow_items_fly_as_their_own_entity_types() {
        let kind =
            |item| ArrowShot::new(ItemStack::mint(item, 1, MintReason::Test), [0.0; 3]).kind();
        assert_eq!(kind(Item::Arrow), EntityType::Arrow);
        assert_eq!(kind(Item::TippedArrow), EntityType::Arrow);
        assert_eq!(kind(Item::SpectralArrow), EntityType::SpectralArrow);
        assert_eq!(kind(Item::Trident), EntityType::Trident);
    }

    #[tuff_core::watchdog]
    #[test]
    fn clipping_finds_where_a_segment_enters_a_box() {
        let unit = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let hit = clip(unit, [-1.0, 0.5, 0.5], [2.0, 0.5, 0.5]).unwrap();
        assert!((hit[0] - 0.0).abs() < 1e-9);
        assert!(clip(unit, [-1.0, 2.0, 0.5], [2.0, 2.0, 0.5]).is_none());
        assert!(
            clip(unit, [0.5, 0.5, 0.5], [2.0, 0.5, 0.5]).is_none(),
            "from inside"
        );
        assert!(
            clip(unit, [-3.0, 0.5, 0.5], [-2.0, 0.5, 0.5]).is_none(),
            "too short"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn rotations_follow_the_flight() {
        let (yaw, pitch) = rotation_of([0.0, 1.0, 1.0]);
        assert!(yaw.abs() < 1e-6);
        assert!((pitch - 45.0).abs() < 1e-4);
        assert!((lerp_rotation(170.0, -170.0) + 186.0).abs() < 1e-4);
    }
}
