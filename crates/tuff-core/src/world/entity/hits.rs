//! Who is near an entity and how a hit lands: players seen by entities, the
//! blame for damage, boss bars and hit details.

use super::*;

/// A player near the region, as entities see it.
#[derive(Debug, Clone, Copy)]
pub struct NearbyPlayer<'a> {
    pub id: PlayerId,
    pub uuid: u128,
    pub name: &'a str,
    pub position: [f64; 3],
    pub eye_height: f64,
    pub yaw: f32,
    pub pitch: f32,
    /// What the main and off hand hold.
    pub held: [Option<Item>; 2],
    pub creative: bool,
    /// The player's box height, which follows its pose.
    pub height: f64,
    pub sneaking: bool,
    pub invisible: bool,
    /// Creative players (and those with the ability) are not attacked.
    pub invulnerable: bool,
    /// Feet, legs, chest and head.
    pub armor: [Option<Item>; 4],
    /// Its active effects, one bit per registry id.
    pub effects: u64,
    /// How far its client last moved it in one tick.
    pub movement: [f64; 3],
    /// The entity of this region it rides.
    pub vehicle: Option<i32>,
    /// Ticks it has lain in bed (capped at 100) while asleep.
    pub sleeping: Option<u32>,
    /// Who hurt it lately (id, stamp) in a way wolves answer.
    pub last_hurt_by: Option<(i32, u64)>,
    /// What it attacked last (id, stamp).
    pub last_hurt_mob: Option<(i32, u64)>,
    /// A tamed parrot may land on its shoulder.
    pub perchable: bool,
    pub sprinting: bool,
}

impl NearbyPlayer<'_> {
    pub fn bounds(&self) -> [f64; 6] {
        let [x, y, z] = self.position;
        [x - 0.3, y, z - 0.3, x + 0.3, y + self.height, z + 0.3]
    }

    pub fn eye(&self) -> [f64; 3] {
        let [x, y, z] = self.position;
        [x, y + self.eye_height, z]
    }

    /// How far away a mob of `kind` notices this player, as a fraction of
    /// its follow range: sneaking, invisibility and heads worn for the mob.
    pub fn visibility(&self, kind: EntityType) -> f64 {
        let mut visibility = 1.0;
        if self.sneaking {
            visibility *= 0.8;
        }
        if self.invisible {
            let worn = self.armor.iter().filter(|a| a.is_some()).count() as f32;
            visibility *= 0.7 * f64::from((worn / 4.0).max(0.1));
        }
        if let Some(head) = self.armor[3]
            && let Some((kinds, factor)) = head.mob_visibility()
            && kinds.contains(&kind)
        {
            visibility *= f64::from(factor);
        }
        visibility.clamp(0.0, 10.0)
    }
}

/// Who caused damage, as a victim sees it: for death messages, retaliation
/// and damage type conditions.
#[derive(Debug, Clone, PartialEq)]
pub struct Culprit {
    pub id: i32,
    pub uuid: u128,
    pub kind: EntityType,
    pub name: TextComponent,
    /// A charged creeper, one of whose victims drops its head.
    pub charged: Option<HeadDrop>,
    /// The online player a tamed attacker belongs to, whose team it takes.
    pub owner_name: Option<String>,
    /// The custom name of the item in its hand, for death messages.
    pub held_name: Option<Box<TextComponent>>,
}

/// The one head a charged creeper's blast may drop, shared by every
/// victim of it.
#[derive(Debug, Clone, Default)]
pub struct HeadDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl HeadDrop {
    /// No victim has dropped its head yet.
    pub fn available(&self) -> bool {
        !self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Takes the head drop; false if another victim already did.
    pub fn claim(&self) -> bool {
        !self.0.swap(true, std::sync::atomic::Ordering::AcqRel)
    }
}

impl PartialEq for HeadDrop {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Culprit {
    pub fn player(id: PlayerId, uuid: u128, name: impl Into<String>) -> Self {
        Self {
            id,
            uuid,
            kind: EntityType::Player,
            name: TextComponent::new(name.into()),
            charged: None,
            owner_name: None,
            held_name: None,
        }
    }

    /// Remembers the weapon's custom name, shown in bracketed form.
    pub fn holding<M: tuff_item::item::Mode>(mut self, weapon: Option<&ItemStack<M>>) -> Self {
        self.held_name = weapon
            .and_then(|stack| stack.components.custom_name.as_deref())
            .map(|name| {
                Box::new(
                    TextComponent::new("[")
                        .append(name.clone())
                        .append(TextComponent::new("]")),
                )
            });
        self
    }

    /// The player's name, when a player is the culprit.
    pub fn player_name(&self) -> Option<&str> {
        (self.kind == EntityType::Player).then_some(self.name.text.as_str())
    }

    /// An entity by its custom name, or its type's name.
    pub fn entity(base: &EntityBase) -> Self {
        let name = base
            .custom_name
            .as_ref()
            .and_then(TextComponent::from_nbt)
            .unwrap_or_else(|| {
                let (namespace, path) = base.kind.name().split_once(':').unwrap_or(("", ""));
                TextComponent::translate(format!("entity.{namespace}.{path}"), [])
            });
        Self {
            id: base.id,
            uuid: base.uuid,
            kind: base.kind,
            name,
            charged: None,
            owner_name: None,
            held_name: None,
        }
    }

    pub fn is_player(&self) -> bool {
        self.kind == EntityType::Player
    }
}

/// A boss health bar a [`RegionEntity`] wants shown to nearby players (see
/// [`RegionEntity::boss_bar`]): the wire fields of the game's Boss Event
/// packet, minus the identifying UUID (the entity's own).
#[derive(Debug, Clone)]
pub struct BossBarState {
    pub name: TextComponent,
    /// 0.0 to 1.0.
    pub progress: f32,
    pub color: i32,
    pub overlay: i32,
    pub flags: u8,
}

/// Damage an entity (a mob's attack, a projectile) does to a player or
/// another entity.
#[derive(Clone, Debug)]
pub struct EntityHit {
    pub amount: f32,
    pub kind: &'static DamageType,
    /// The entity to blame (the shooter of an arrow).
    pub attacker: Option<Culprit>,
    /// The entity that hit, when it is not the attacker (a projectile).
    pub projectile: Option<EntityType>,
    /// The entity id of that projectile.
    pub projectile_id: Option<i32>,
    /// Where the hit comes from, for knockback and shields.
    pub position: Option<[f64; 3]>,
    /// Knockback on top of the usual push, and its direction (x, z).
    pub knockback: Option<(f64, f64, f64)>,
    /// A push added to the victim's motion, its horizontal part reduced by
    /// knockback resistance (a Punch arrow).
    pub push: Option<[f64; 3]>,
    /// Set when `push` is a throw worked out for this power: the victim's
    /// knockback resistance is taken off the power, not multiplied in.
    pub push_power: Option<f64>,
    /// The attacker wants a `Notice::HitLanded` if this hurts the victim.
    pub report_landed: bool,
    /// The attacker wants a `Notice::ShieldBlocked` if a raised shield
    /// stops this hit.
    pub report_blocked: bool,
    /// Seconds of fire the hit sets the victim alight for, if it lands.
    pub ignite: f32,
    /// The attacker's weapon: its enchantments act on the victim.
    pub weapon: Option<tuff_item::item::ItemSnapshot>,
    /// The part hit, for entities made of parts.
    pub part: Option<usize>,
    /// Status effects the victim gets if the hit lands (a wither
    /// skeleton's wither, a stray's slowness arrow).
    pub effects: Vec<crate::world::effects::ActiveEffect>,
    /// An arrow that stays stuck in the victim if the hit lands.
    pub sticks: bool,
    /// The projectile, bounced back, if the hit fails to hurt.
    pub rebound: Option<Rebound>,
    /// How fast the projectile flew when it hit.
    pub projectile_velocity: Option<[f64; 3]>,
}
