//! Melee attack and damage filters plugins declare while registering: the
//! server cancels or scales matching hits without running guest code and
//! reports them as events.

use std::future::Future;
use std::pin::Pin;

use tuff_core::permission::Subject;
use tuff_core::protocol::play::Serverbound;
use tuff_core::world::entity::RegionEntity;
use tuff_core::world::inventory::Inventory;
use tuff_core::world::region_api::{RegionHooks, RegionWorld};
use tuff_core::world::session::hooks::{PacketInterceptor, SessionHooks, Verdict};
use tuff_core::world::session::{DamageSource, PlayerSession};
use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::pos::ChunkPos;

use crate::bridge::{entity_of, player};
use crate::host::WasmHost;
use crate::portal::qualified;
use crate::sandbox::{uuid, uuid_of};
use crate::wit::bindings::types as wit;
use crate::wit::bindings::types::{
    DamageFiltered, DamageVictim, DamagedEntity, Event, EventKinds, PlayerAttack,
};

/// Attack filters and damage filters one plugin may declare, each.
pub const MAX_FILTERS: usize = 32;

/// Two corners, in any order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    min: [f64; 3],
    max: [f64; 3],
}

impl Area {
    fn new(corners: (wit::Vec3, wit::Vec3)) -> Self {
        let (a, b) = corners;
        Self {
            min: [a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)],
            max: [a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)],
        }
    }

    fn contains(&self, at: [f64; 3]) -> bool {
        (0..3).all(|i| at[i] >= self.min[i] && at[i] <= self.max[i])
    }
}

/// Where a click, item or entity filter applies, and who it spares.
#[derive(Debug, Clone, PartialEq)]
pub struct Scope {
    dimension: Option<String>,
    area: Option<Area>,
    bypass: Option<String>,
}

impl Scope {
    pub fn parse(scope: wit::FilterScope) -> Self {
        Self {
            dimension: scope.dimension,
            area: scope.area.map(Area::new),
            bypass: scope.bypass,
        }
    }

    /// Whether the filter applies to `session`'s player, testing the area
    /// against `at`.
    pub fn applies(&self, session: &PlayerSession, at: [f64; 3]) -> bool {
        within(&self.area, at)
            && self
                .dimension
                .as_deref()
                .is_none_or(|wanted| session.shared.dimensions.get(session.dimension).key == wanted)
            && !self.bypass.as_deref().is_some_and(|node| {
                session
                    .shared
                    .permissions
                    .view()
                    .decide(
                        Subject::Player {
                            uuid: session.profile.id,
                            level: session.mode.permission,
                        },
                        node,
                    )
                    .held()
            })
    }

    pub fn applies_to_player(&self, session: &PlayerSession) -> bool {
        self.applies(session, session.pose.position)
    }
}

fn within(area: &Option<Area>, at: [f64; 3]) -> bool {
    area.as_ref().is_none_or(|area| area.contains(at))
}

fn same<T: PartialEq>(wanted: &Option<T>, got: &T) -> bool {
    wanted.as_ref().is_none_or(|wanted| wanted == got)
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttackFilter {
    pub kind: Option<String>,
    pub tag: Option<String>,
    pub held_item: Option<String>,
    pub attacker: Option<u128>,
    pub dimension: Option<String>,
    pub area: Option<Area>,
}

impl AttackFilter {
    pub fn parse(filter: wit::AttackFilter) -> Self {
        Self {
            kind: filter.kind.map(|kind| qualified(&kind)),
            tag: filter.tag,
            held_item: filter.held_item.map(|item| qualified(&item)),
            attacker: filter.attacker.map(uuid_of),
            dimension: filter.dimension,
            area: filter.area.map(Area::new),
        }
    }

    fn matches(&self, hit: &Attack<'_>) -> bool {
        same(&self.kind, &hit.kind.to_owned())
            && self
                .tag
                .as_deref()
                .is_none_or(|wanted| hit.tags.iter().any(|tag| tag == wanted))
            && self
                .held_item
                .as_deref()
                .is_none_or(|wanted| hit.held == Some(wanted))
            && same(&self.attacker, &hit.attacker)
            && self
                .dimension
                .as_deref()
                .is_none_or(|wanted| wanted == hit.dimension)
            && within(&self.area, hit.at)
    }
}

struct Attack<'a> {
    kind: &'a str,
    tags: &'a [String],
    held: Option<&'a str>,
    attacker: u128,
    dimension: &'a str,
    at: [f64; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct DamageFilter {
    pub victim: Option<u128>,
    pub causes: Vec<String>,
    pub attacker: Option<u128>,
    pub attacker_kind: Option<String>,
    pub dimension: Option<String>,
    pub area: Option<Area>,
    pub scale: f32,
}

impl DamageFilter {
    pub fn parse(filter: wit::DamageFilter) -> Result<Self, String> {
        if !filter.scale.is_finite() || filter.scale < 0.0 {
            return Err("scale must be a finite number, not below 0".into());
        }
        Ok(Self {
            victim: filter.victim.map(uuid_of),
            causes: filter.causes.iter().map(|c| qualified(c)).collect(),
            attacker: filter.attacker.map(uuid_of),
            attacker_kind: filter.attacker_kind.map(|kind| qualified(&kind)),
            dimension: filter.dimension,
            area: filter.area.map(Area::new),
            scale: filter.scale,
        })
    }

    fn matches(&self, victim: &Victim<'_>, cause: &Cause<'_>) -> bool {
        if !self.causes.is_empty() && !self.causes.iter().any(|wanted| wanted == cause.kind) {
            return false;
        }
        if !same(&self.victim, &victim.uuid) {
            return false;
        }
        if self.attacker.is_some() && cause.attacker.map(|a| a.0) != self.attacker {
            return false;
        }
        if let Some(kind) = &self.attacker_kind
            && cause.attacker.is_none_or(|a| a.1 != kind)
        {
            return false;
        }
        if let Some(dimension) = &self.dimension
            && victim.dimension != dimension
        {
            return false;
        }
        within(&self.area, victim.at)
    }
}

/// Who is being hurt.
struct Victim<'a> {
    uuid: u128,
    dimension: &'a str,
    at: [f64; 3],
}

/// What hurts: the damage type id and the attacker's uuid and type id.
struct Cause<'a> {
    kind: &'a str,
    attacker: Option<(u128, &'a str)>,
}

fn first_scale(
    snapshot: &crate::host::Index,
    victim: &Victim<'_>,
    cause: &Cause<'_>,
) -> Option<f32> {
    snapshot
        .damage_filters
        .iter()
        .find(|filter| filter.matches(victim, cause))
        .map(|filter| filter.scale)
}

/// Cancels matching attacks on non-player entities and reports them.
pub struct Attacks;

impl PacketInterceptor for Attacks {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>> {
        Box::pin(async move {
            match packet {
                Serverbound::Attack(target) => attack(session, *target).await,
                _ => Verdict::Allow,
            }
        })
    }
}

async fn attack(session: &mut PlayerSession, target: i32) -> Verdict {
    let shared = session.shared.clone();
    let Some(host) = shared.services.get::<WasmHost>() else {
        return Verdict::Allow;
    };
    let filters = host.snapshot().attack_filters.clone();
    let reporting = host.listens(EventKinds::PLAYER_ATTACK);
    if filters.is_empty() && !reporting {
        return Verdict::Allow;
    }
    let (kind, at, uuid, tags) =
        if let Some((at, handle)) = session.view.entities.visible_player(target) {
            let region = ChunkPos::of_position(at).region();
            let index = session.shared.players.borrow().clone();
            let Some(victim) = index
                .near(session.dimension, region)
                .iter()
                .find(|p| p.handle.id == handle.id)
            else {
                return Verdict::Allow;
            };
            (EntityType::Player, at, uuid(victim.uuid), Vec::new())
        } else {
            let Some((kind, chunk, at)) = session.view.entities.visible_entity(target) else {
                return Verdict::Allow;
            };
            let Some(region) = session.region(chunk.region()).await else {
                return Verdict::Allow;
            };
            let Some(Some((entity, tags))) = region
                .ask(move |world| {
                    world.entity(target).map(|tracked| {
                        (
                            entity_of(&*world, &*tracked.entity),
                            tracked.entity.base().tags.clone(),
                        )
                    })
                })
                .await
            else {
                return Verdict::Allow;
            };
            (kind, at, entity.uuid, tags)
        };
    let held = session.inventory.slots()[Inventory::hotbar_slot(session.inventory.selected())]
        .as_ref()
        .map(|stack| stack.item().name());
    let dimension = session.shared.dimensions.get(session.dimension).key.clone();
    let hit = Attack {
        kind: kind.name(),
        tags: &tags,
        held,
        attacker: session.profile.id,
        dimension: &dimension,
        at,
    };
    let cancelled = filters.iter().any(|filter| filter.matches(&hit));
    if reporting {
        let who = player(session);
        host.event(
            Event::PlayerAttack(PlayerAttack {
                player: who.to_wit(),
                victim: uuid,
                kind: kind.name().to_owned(),
                cancelled,
            }),
            Some(who),
        );
    }
    if cancelled {
        Verdict::Cancel
    } else {
        Verdict::Allow
    }
}

/// Cancels and scales damage to players and mobs as filters say.
pub struct Damage;

fn attacker_of(source: &DamageSource) -> Option<(u128, &'static str)> {
    source.attacker.as_ref().map(|a| (a.uuid, a.kind.name()))
}

fn player_scale(session: &PlayerSession, source: &DamageSource, report: bool) -> Option<f32> {
    let host = session.shared.services.get::<WasmHost>()?;
    let snapshot = host.snapshot();
    if snapshot.damage_filters.is_empty() {
        return None;
    }
    let dimension = &session.shared.dimensions.get(session.dimension).key;
    let victim = Victim {
        uuid: session.profile.id,
        dimension,
        at: session.pose.position,
    };
    let cause = Cause {
        kind: source.kind.name,
        attacker: attacker_of(source),
    };
    let scale = first_scale(&snapshot, &victim, &cause)?;
    if report && host.listens(EventKinds::DAMAGE_FILTERED) {
        let who = player(session);
        host.event(
            Event::DamageFiltered(DamageFiltered {
                victim: DamageVictim::Player(who.to_wit()),
                damage_type: source.kind.name.to_owned(),
                attacker: source.attacker.as_ref().map(|a| uuid(a.uuid)),
                scale,
            }),
            Some(who),
        );
    }
    Some(scale)
}

impl SessionHooks for Damage {
    fn allows_player_hurt(&self, session: &PlayerSession, source: &DamageSource) -> bool {
        match player_scale(session, source, true) {
            Some(scale) => scale > 0.0,
            None => true,
        }
    }

    fn player_hurt_scale(&self, session: &PlayerSession, source: &DamageSource) -> f32 {
        match player_scale(session, source, false) {
            Some(scale) if scale > 0.0 => scale,
            _ => 1.0,
        }
    }
}

impl RegionHooks for Damage {
    fn entity_hurt_scale(
        &self,
        world: &dyn RegionWorld,
        victim: &dyn RegionEntity,
        kind: &'static DamageType,
        attacker: Option<(u128, EntityType)>,
    ) -> f32 {
        let Some(host) = world.shared().services.get::<WasmHost>() else {
            return 1.0;
        };
        let snapshot = host.snapshot();
        let base = victim.base();
        let victim = Victim {
            uuid: base.uuid,
            dimension: &world.dimension_info().key,
            at: base.position,
        };
        let cause = Cause {
            kind: kind.name,
            attacker: attacker.map(|(uuid, kind)| (uuid, kind.name())),
        };
        let Some(scale) = first_scale(&snapshot, &victim, &cause) else {
            return 1.0;
        };
        if host.listens(EventKinds::DAMAGE_FILTERED) {
            host.event(
                Event::DamageFiltered(DamageFiltered {
                    victim: DamageVictim::Entity(DamagedEntity {
                        uuid: uuid(victim.uuid),
                        kind: base.kind.name().to_owned(),
                    }),
                    damage_type: kind.name.to_owned(),
                    attacker: attacker.map(|(id, _)| uuid(id)),
                    scale,
                }),
                None,
            );
        }
        scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec3(x: f64, y: f64, z: f64) -> wit::Vec3 {
        wit::Vec3 { x, y, z }
    }

    fn filter() -> AttackFilter {
        AttackFilter::parse(wit::AttackFilter {
            kind: Some("zombie".into()),
            tag: Some("arena".into()),
            held_item: Some("stick".into()),
            attacker: None,
            dimension: Some("minecraft:overworld".into()),
            area: Some((vec3(10.0, 0.0, 10.0), vec3(-10.0, 20.0, -10.0))),
        })
    }

    fn attack(held: Option<&'static str>, at: [f64; 3]) -> bool {
        let tags = vec!["arena".to_owned()];
        filter().matches(&Attack {
            kind: "minecraft:zombie",
            tags: &tags,
            held,
            attacker: 1,
            dimension: "minecraft:overworld",
            at,
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn attack_filters_need_every_set_field() {
        assert!(attack(Some("minecraft:stick"), [0.0, 5.0, 0.0]));
        assert!(!attack(Some("minecraft:stick"), [11.0, 5.0, 0.0]));
        assert!(!attack(None, [0.0, 5.0, 0.0]));
        assert!(!attack(Some("minecraft:dirt"), [0.0, 5.0, 0.0]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn damage_scales_below_zero_or_not_finite_are_refused() {
        let make = |scale| {
            DamageFilter::parse(wit::DamageFilter {
                victim: None,
                causes: Vec::new(),
                attacker: None,
                attacker_kind: None,
                dimension: None,
                area: None,
                scale,
            })
        };
        assert!(make(-0.1).is_err());
        assert!(make(f32::NAN).is_err());
        assert!(make(0.0).is_ok());
    }
}
