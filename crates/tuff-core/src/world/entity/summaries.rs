use super::*;

/// Arrows stuck in a living entity, which work loose one at a time, the
/// faster the more there are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StuckArrows {
    pub count: i32,
    pub(super) timer: i32,
}

impl StuckArrows {
    /// One tick; true when an arrow fell out.
    pub fn tick(&mut self) -> bool {
        if self.count <= 0 {
            return false;
        }
        if self.timer <= 0 {
            self.timer = 20 * (30 - self.count);
        }
        self.timer -= 1;
        if self.timer <= 0 {
            self.count -= 1;
            return true;
        }
        false
    }
}

/// A projectile that flies back off a victim its hit failed to hurt (a
/// raised shield, a fresh hit's invulnerability). The victim's owner
/// spawns it; once taken it is gone, so copies of the hit share it.
#[derive(Clone)]
pub struct Rebound(
    std::sync::Arc<std::sync::Mutex<Option<Box<dyn RegionEntity>>>>,
    Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
);

impl Rebound {
    pub fn new(entity: Box<dyn RegionEntity>) -> Self {
        Self(
            std::sync::Arc::new(std::sync::Mutex::new(Some(entity))),
            None,
        )
    }

    /// A rebound that stands in for a projectile still flying on (a
    /// piercing arrow): of all rebounds sharing `claim`, only the first
    /// taken comes back, and the claim tells the projectile it bounced.
    pub fn claiming(
        entity: Box<dyn RegionEntity>,
        claim: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self(
            std::sync::Arc::new(std::sync::Mutex::new(Some(entity))),
            Some(claim),
        )
    }

    pub fn take(&self) -> Option<Box<dyn RegionEntity>> {
        let entity = self.0.lock().ok()?.take()?;
        if let Some(claim) = &self.1
            && claim.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return None;
        }
        Some(entity)
    }
}

impl Debug for Rebound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Rebound")
    }
}

impl EntityHit {
    pub fn new(amount: f32, kind: &'static DamageType) -> Self {
        Self {
            amount,
            kind,
            attacker: None,
            projectile: None,
            projectile_id: None,
            position: None,
            knockback: None,
            push: None,
            push_power: None,
            report_landed: false,
            report_blocked: false,
            ignite: 0.0,
            weapon: None,
            part: None,
            effects: Vec::new(),
            sticks: false,
            rebound: None,
            projectile_velocity: None,
        }
    }
}

/// Work an entity hands to the regions next to its own.
#[derive(Clone)]
pub struct NeighbourTask(pub NeighbourWork);

impl Debug for NeighbourTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NeighbourTask")
    }
}

/// Where and on what a projectile landed.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectileLanding {
    pub id: i32,
    pub uuid: u128,
    pub kind: EntityType,
    pub position: [f64; 3],
    /// Who shot or threw it, if anyone.
    pub owner: Option<Culprit>,
    /// The block it struck.
    pub block: Option<BlockPos>,
    /// The entity it struck, by entity id.
    pub target: Option<i32>,
}

/// A projectile as a block it struck sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectileImpact {
    pub kind: EntityType,
    /// An arrow or trident: target blocks hold their signal longer.
    pub arrow: bool,
    pub on_fire: bool,
    pub velocity: [f64; 3],
    /// Who shot or threw it, if anyone.
    pub owner: Option<Culprit>,
}

impl ProjectileImpact {
    /// Whether it may change the block at all: a player's always, anyone
    /// else's only while mobs may grief.
    pub fn may_interact(&self, shared: &crate::world::Shared) -> bool {
        match &self.owner {
            Some(owner) if owner.is_player() => true,
            Some(_) => shared.rule_bool("mob_griefing"),
            None => true,
        }
    }

    /// Whether it may break the block it struck.
    pub fn may_break(&self, shared: &crate::world::Shared) -> bool {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::entity_type::key::IMPACT_PROJECTILES,
            self.kind,
        ) && shared.rule_bool("projectiles_can_break_blocks")
    }

    pub fn speed(&self) -> f64 {
        self.velocity.iter().map(|c| c * c).sum::<f64>().sqrt()
    }
}

/// Another entity of the region, as the one ticking sees it.
#[derive(Debug, Clone, Copy)]
pub struct EntitySummary {
    pub id: i32,
    pub uuid: u128,
    pub kind: EntityType,
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub yaw: f32,
    pub bounds: [f64; 6],
    pub living: Option<LivingSummary>,
    /// Projectiles hit it though it is not alive.
    pub pickable: bool,
    /// For a dropped item: what it holds, and whether a mob may take it
    /// now (no pickup delay, not offered to a player).
    pub item: Option<(tuff_vanilla_data::Item, u8, bool)>,
    /// What it rides, by id.
    pub vehicle: Option<i32>,
    /// Something rides it.
    pub carries: bool,
    /// A dropped item that is an ominous banner (a white banner with eight layers).
    pub ominous_banner: bool,
    /// A sheep with blue wool (an evoker's wololo target).
    pub blue_sheep: bool,
}

pub(super) const GRID_CELL: f64 = 4.0;
pub(super) const GRID_REACH: f64 = 1.5;

/// Buckets a region's [`EntitySummary`] slice by horizontal cell so an
/// overlap scan visits only the neighbourhood. Indices refer to the slice
/// it was built from; entities too wide for the cell margin stay in an
/// always-scanned list.
#[derive(Debug, Default)]
pub struct EntityGrid {
    pub(super) cells: HashMap<(i32, i32), Vec<u32>>,
    pub(super) wide: Vec<u32>,
    pub(super) placed: Vec<Option<(i32, i32)>>,
}

impl EntityGrid {
    pub fn build(summaries: &[EntitySummary]) -> Self {
        let mut grid = Self {
            placed: vec![None; summaries.len()],
            ..Self::default()
        };
        for (index, summary) in summaries.iter().enumerate() {
            grid.insert(index, summary);
        }
        grid
    }

    pub(super) fn cell(bounds: &[f64; 6]) -> Option<(i32, i32)> {
        let narrow =
            bounds[3] - bounds[0] <= 2.0 * GRID_REACH && bounds[5] - bounds[2] <= 2.0 * GRID_REACH;
        narrow.then(|| {
            (
                (((bounds[0] + bounds[3]) * 0.5) / GRID_CELL).floor() as i32,
                (((bounds[2] + bounds[5]) * 0.5) / GRID_CELL).floor() as i32,
            )
        })
    }

    pub(super) fn insert(&mut self, index: usize, summary: &EntitySummary) {
        let cell = Self::cell(&summary.bounds);
        self.placed[index] = cell;
        let list = match cell {
            Some(cell) => self.cells.entry(cell).or_default(),
            None => &mut self.wide,
        };
        let at = list.partition_point(|i| (*i as usize) < index);
        list.insert(at, index as u32);
    }

    pub(super) fn remove(&mut self, index: usize) {
        let list = match self.placed[index] {
            Some(cell) => self.cells.get_mut(&cell),
            None => Some(&mut self.wide),
        };
        if let Some(list) = list
            && let Ok(at) = list.binary_search(&(index as u32))
        {
            list.remove(at);
        }
    }

    /// The slot at `index` now holds `summary`.
    pub fn replace(&mut self, index: usize, summary: &EntitySummary) {
        if self.placed.get(index) == Some(&Self::cell(&summary.bounds)) {
            return;
        }
        self.remove(index);
        self.insert(index, summary);
    }

    /// Indices, ascending, of every entity whose bounds may overlap `bounds`.
    pub fn candidates(&self, bounds: &[f64; 6]) -> Vec<u32> {
        let cell = |v: f64| (v / GRID_CELL).floor() as i32;
        let (x0, x1) = (cell(bounds[0] - GRID_REACH), cell(bounds[3] + GRID_REACH));
        let (z0, z1) = (cell(bounds[2] - GRID_REACH), cell(bounds[5] + GRID_REACH));
        let mut out = self.wide.clone();
        for x in x0..=x1 {
            for z in z0..=z1 {
                if let Some(list) = self.cells.get(&(x, z)) {
                    out.extend_from_slice(list);
                }
            }
        }
        // Each list is ascending and they are disjoint, so the stable
        // sort only merges runs.
        out.sort();
        out
    }
}

impl<'a> TickContext<'a> {
    /// Up to `limit` of the region's other entities whose bounds overlap
    /// `bounds`, scanning from an offset picked by `seed` instead of in
    /// slice order, for crowds where a sample of neighbours will do.
    pub fn entities_overlapping_sample(
        &self,
        bounds: &[f64; 6],
        seed: u64,
        limit: usize,
    ) -> Vec<&'a EntitySummary> {
        let entities = self.entities;
        let overlaps = |e: &&EntitySummary| crate::world::physics::intersects(e.bounds, *bounds);
        let Some(grid) = self.entity_grid else {
            let len = entities.len().max(1);
            let at = (seed % len as u64) as usize;
            return entities[at..]
                .iter()
                .chain(&entities[..at])
                .filter(overlaps)
                .take(limit)
                .collect();
        };
        let cell = |v: f64| (v / GRID_CELL).floor() as i32;
        let (x0, x1) = (cell(bounds[0] - GRID_REACH), cell(bounds[3] + GRID_REACH));
        let (z0, z1) = (cell(bounds[2] - GRID_REACH), cell(bounds[5] + GRID_REACH));
        let mut lists: Vec<&[u32]> = vec![&grid.wide];
        for x in x0..=x1 {
            for z in z0..=z1 {
                if let Some(list) = grid.cells.get(&(x, z)) {
                    lists.push(list);
                }
            }
        }
        let len: usize = lists.iter().map(|l| l.len()).sum();
        if len == 0 {
            return Vec::new();
        }
        let at = (seed % len as u64) as usize;
        let all = || lists.iter().flat_map(|l| l.iter());
        all()
            .skip(at)
            .chain(all().take(at))
            .map(|i| &entities[*i as usize])
            .filter(overlaps)
            .take(limit)
            .collect()
    }

    /// The region's other entities whose bounds may overlap `bounds`.
    pub fn entities_near(&self, bounds: &[f64; 6]) -> Vec<&'a EntitySummary> {
        let entities = self.entities;
        match self.entity_grid {
            Some(grid) => grid
                .candidates(bounds)
                .into_iter()
                .map(|i| &entities[i as usize])
                .collect(),
            None => entities.iter().collect(),
        }
    }
}

/// One part of an entity made of parts, as projectiles see it.
#[derive(Debug, Clone, Copy)]
pub struct PartBox {
    /// The id clients know the part by.
    pub id: i32,
    /// The entity it belongs to, by id and type.
    pub owner: i32,
    pub kind: EntityType,
    pub bounds: [f64; 6],
    pub position: [f64; 3],
}

/// What mobs know of each other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LivingSummary {
    pub alive: bool,
    /// What it is attacking.
    pub target: Option<i32>,
    /// Negative while a baby.
    pub age: i32,
    pub in_love: bool,
    pub panicking: bool,
    pub eye_height: f32,
    /// Touching water.
    pub in_water: bool,
    /// Mobs may pick it as a target (an axolotl playing dead may not).
    pub seen_as_enemy: bool,
    /// It is an enemy of the player (hostile mobs, slimes, ghasts).
    pub enemy: bool,
    pub health: f32,
    /// A tamed animal (whose owner may be away).
    pub tamed: bool,
    /// The player that tamed it.
    pub owner: Option<u128>,
    /// The baby villager it is chasing at play.
    pub chasing: Option<i32>,
    /// Others neither push it nor count it toward cramming: it climbs,
    /// carries a rider (horses), or never takes pushes (bats).
    pub unpushable: bool,
}

/// What `/debugpath` found for an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugPath {
    NotMob,
    NoPath,
    NotComplete,
    Made,
}

/// What mobs may ask about a container besides its items.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContainerInfo {
    /// Its lock refuses an empty hand.
    pub locked: bool,
    /// Players and mobs with it open now.
    pub openers: usize,
}

/// What an entity may look at and use while it ticks.
pub struct TickContext<'a> {
    pub tick: u64,
    /// The dimension the region belongs to.
    pub dimension: DimensionId,
    /// The block at a position, `None` where the region cannot see.
    pub block: &'a dyn Fn(BlockPos) -> Option<BlockState>,
    /// Sky and block light at a position, `None` where unknown.
    pub light: &'a dyn Fn(BlockPos) -> Option<(u8, u8)>,
    /// How much the sky light is darkened now (night, weather).
    pub sky_darken: u8,
    /// The dimension's ambient light, which brightens every light level.
    pub ambient_light: f32,
    /// Undead mobs burn in the sun now.
    pub monsters_burn: bool,
    pub raining: bool,
    /// The biome at a position, `None` where the region cannot see.
    pub biome: &'a dyn Fn(BlockPos) -> Option<&'static str>,
    /// A container's item slots at a position (read-only; `None` where
    /// there is none). A double chest reads as both halves in menu order.
    pub container: &'a dyn Fn(BlockPos) -> Option<std::borrow::Cow<'a, [Option<ItemStack>]>>,
    /// Whether the container at a position is locked and how many players
    /// and mobs have it open.
    pub container_info: &'a dyn Fn(BlockPos) -> ContainerInfo,
    /// Ticks players have spent near a loaded chunk (zero elsewhere).
    pub inhabited_time: &'a dyn Fn(ChunkPos) -> u64,
    pub min_y: i32,
    pub max_y: i32,
    /// Living players present in or near the region (within the player
    /// index's reach), who can be seen, followed and hit.
    pub players: &'a [NearbyPlayer<'a>],
    /// Living players anywhere in the dimension.
    pub players_in_dimension: usize,
    /// The region's other entities.
    pub entities: &'a [EntitySummary],
    /// Buckets of `entities` when the region keeps them current.
    pub entity_grid: Option<&'a EntityGrid>,
    /// Where the blocks pistons are moving are now: collision boxes the
    /// block lookup cannot give.
    pub pistons: &'a [crate::world::physics::Aabb],
    /// The parts of entities made of parts, which projectiles hit by the
    /// ids clients give them.
    pub parts: &'a [PartBox],
    /// World-wide state: game rules, difficulty, dimensions.
    pub shared: &'a Shared,
    pub rng: &'a mut StdRng,
    pub effects: &'a mut Vec<Effect>,
    /// Path search nodes left for this tick in the region.
    pub path_budget: &'a mut u32,
    /// The region's points of interest and how claimed they are.
    pub pois: &'a crate::world::poi::PoiStore,
    /// The region's feature state (see `region_api::RegionState`), where a
    /// feature's entities keep what they share within the region.
    pub state: &'a crate::world::region_api::RegionStateLock,
}

/// The key a placed entity's data carries while the chunk is generated
/// when it should be set up as a fresh structure spawn; the world takes it
/// out before loading the entity.
pub const FINALIZE_SPAWN: &str = "FinalizeSpawn";

/// An outstanding pickup offer: the player it went to, the tick it was
/// made and its id. Only an answer naming this id is honoured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    pub player: crate::world::messages::PlayerId,
    pub at: u64,
    pub offer: u64,
}

/// A pickup offer id unique across every region, so an answer can never
/// be mistaken for a later offer of the same entity, wherever it went.
pub fn next_offer_id() -> u64 {
    pub(super) static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// An entity lying where players can pick it up.
pub struct GroundPickup<'a> {
    pub reserved: &'a mut Option<Reservation>,
    /// Players who turned it down since it was last offered to everyone
    /// touching it.
    pub refused_by: &'a mut Vec<crate::world::messages::PlayerId>,
    pub bounds: [f64; 6],
    /// Only creative players take it, and they get nothing.
    pub creative_only: bool,
    /// What a player taking it gets.
    pub stack: Option<ItemStack>,
    /// Only the player with this UUID may take it (a thrown trident).
    pub only: Option<u128>,
}

/// A player's melee hit on an entity, as the entity's region applies it.
#[derive(Debug, Clone)]
pub struct EntityAttack {
    pub attacker: PlayerId,
    pub attacker_uuid: u128,
    pub attacker_position: [f64; 3],
    pub attacker_yaw: f32,
    pub attacker_pitch: f32,
    pub damage: f32,
    pub knockback: f32,
    pub weapon: Option<ItemStack>,
    pub critical: bool,
    pub magic: bool,
    pub sweep: bool,
    /// The part the player hit, for entities made of parts.
    pub part: Option<usize>,
}

/// What became of an attack that reached an entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttackOutcome {
    pub hurt: bool,
    /// Health lost, for the damage indicator particles.
    pub health_lost: f32,
}

/// A player's right click on an entity.
#[derive(Debug, Clone)]
pub struct EntityInteraction {
    pub player: PlayerId,
    pub player_uuid: u128,
    pub position: [f64; 3],
    /// Where on the entity the player clicked, relative to its position.
    pub location: [f64; 3],
    pub stack: Option<ItemStack>,
    pub off_hand: bool,
    pub creative: bool,
    pub sneaking: bool,
    /// The player already rides something.
    pub riding: bool,
    /// The amplifier of the player's Hero of the Village effect.
    pub hero: Option<u8>,
}

/// The effect of a click on the clicking player's hand, or `None` if the
/// entity ignored it.
pub type InteractOutcome = Option<crate::world::messages::HandEffect>;

/// What became of an entity after its tick.
#[derive(Debug)]
pub enum Tick {
    Alive,
    /// Gone without a trace (despawned).
    Discard,
    /// Gone, setting off an explosion.
    Explode(Explosion),
}
