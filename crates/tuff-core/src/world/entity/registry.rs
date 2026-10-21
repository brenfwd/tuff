use super::*;

/// A block lookup that answers `None` where the caller cannot see.
pub type BlockLookup<'a> = &'a dyn Fn(BlockPos) -> Option<BlockState>;

/// Why a mob is spawning, which some rules care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnReason {
    Natural,
    ChunkGeneration,
    /// A mob spawner block: no ground needed under the mob.
    Spawner,
    /// A trial spawner: like [`Self::Spawner`], and monsters ignore the
    /// light level too.
    TrialSpawner,
    /// A zombie calling for help.
    Reinforcement,
    /// Structure generation placed it.
    Structure,
}

/// Why the mob being set up spawns, kept in the region's state while it is
/// finalized; `None` for every other way in.
#[derive(Debug, Default)]
pub struct SettingUp(pub Option<SpawnReason>);

impl SpawnReason {
    pub fn is_spawner(self) -> bool {
        matches!(self, Self::Spawner | Self::TrialSpawner)
    }

    pub fn ignores_light(self) -> bool {
        self == Self::TrialSpawner
    }
}

/// What spawn rules may look at.
pub struct SpawnLevel<'a> {
    pub block: BlockLookup<'a>,
    /// Sky and block light, `None` where unknown.
    pub light: &'a dyn Fn(BlockPos) -> Option<(u8, u8)>,
    pub sky_darken: u8,
    pub min_y: i32,
    /// The dimension generator's sea level.
    pub sea_level: i32,
    pub dimension: &'static tuff_vanilla_data::dimension_type::DimensionType,
    pub thundering: bool,
    /// The biome at a position, `None` where unknown.
    pub biome: &'a dyn Fn(BlockPos) -> Option<&'static str>,
    /// The world seed (slime chunks).
    pub seed: i64,
    /// The chance slimes may spawn on swamp surfaces now.
    pub surface_slime_chance: f32,
    pub difficulty: crate::world::player::Difficulty,
    /// Whether a player is within the distance of a point.
    pub player_within: &'a dyn Fn([f64; 3], f64) -> bool,
}

impl SpawnLevel<'_> {
    /// The brightest of sky light darkened by `darken` and block light.
    pub fn raw_brightness(&self, pos: BlockPos, darken: u8) -> u8 {
        let (sky, block) = (self.light)(pos).unwrap_or((0, 0));
        sky.saturating_sub(darken).max(block)
    }
}

/// Whether the type's spawn rule holds at `pos`, as the region sees it;
/// the rule draws from its own random seeded with `seed`.
pub fn rule_holds(
    world: &dyn crate::world::region_api::RegionWorld,
    seed: u64,
    kind: EntityType,
    pos: BlockPos,
    rule: SpawnRule,
    reason: SpawnReason,
) -> bool {
    let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(seed);
    let shared = world.shared();
    let block = |p| world.block(p);
    let light = |p: BlockPos| world.engine_light(p);
    let weather = *shared.weather.borrow();
    let biome = |p: BlockPos| world.biome_name(p);
    let dimension = world.dimension_info();
    let present = world.present_players();
    let player_within = |at: [f64; 3], range: f64| {
        present.iter().any(|p| {
            let d: f64 = (0..3).map(|i| (p.position[i] - at[i]).powi(2)).sum();
            d < range * range
        })
    };
    let level = SpawnLevel {
        block: &block,
        light: &light,
        sky_darken: crate::world::sky_darken(shared, dimension),
        min_y: dimension.shape.min_y,
        sea_level: dimension.sea_level(),
        dimension: dimension.kind,
        thundering: weather.thunder_level * weather.rain_level > 0.9,
        biome: &biome,
        seed: shared.seed,
        surface_slime_chance: crate::world::surface_slime_chance(shared, dimension),
        difficulty: *shared.difficulty.borrow(),
        player_within: &player_within,
    };
    rule(kind, &level, pos, reason, &mut rng)
}

/// A type's own rule on top of placement; registered with its loader.
pub type SpawnRule =
    fn(EntityType, &SpawnLevel, BlockPos, SpawnReason, &mut rand::rngs::StdRng) -> bool;

/// Builds a type from its saved fields: `nbt` holds what [`EntityBase`] did
/// not take, and a loader removes what it models. `None` keeps the entity
/// as raw saved data instead.
pub type Loader = fn(EntityBase, &mut Compound) -> Option<Box<dyn RegionEntity>>;

/// Builds the mob a bucket held from its `bucket_entity_data` and the
/// bucket's own components; `base` is new, at where it is released.
pub type BucketRelease =
    fn(EntityBase, Option<Compound>, Option<&Compound>) -> Option<Box<dyn RegionEntity>>;

/// Loaders by saved type id, the spawn rules of types that spawn
/// naturally, how types that live in buckets come out of one, and how
/// projectiles are launched.
#[derive(Default, Clone)]
pub struct Registry {
    loaders: HashMap<String, (EntityType, Loader)>,
    spawn_rules: HashMap<EntityType, SpawnRule>,
    bucket_releases: HashMap<EntityType, BucketRelease>,
    launchers: HashMap<EntityType, crate::world::projectile::Launcher>,
}

impl Registry {
    pub fn register(&mut self, kind: EntityType, loader: Loader) {
        self.register_as(kind.name(), kind, loader);
    }

    /// Registers a saved type id that is sent to clients as `kind`.
    pub fn register_as(&mut self, id: impl Into<String>, kind: EntityType, loader: Loader) {
        self.loaders.insert(id.into(), (kind, loader));
    }

    /// Every type a registered id is sent to clients as, once each.
    pub fn kinds(&self) -> Vec<EntityType> {
        let mut kinds: Vec<EntityType> = self.loaders.values().map(|(kind, _)| *kind).collect();
        kinds.sort_unstable_by_key(|kind| kind.id());
        kinds.dedup();
        kinds
    }

    /// Every registered saved type id.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.loaders.keys().map(String::as_str)
    }

    pub fn knows(&self, id: &str) -> bool {
        self.loaders.contains_key(id)
    }

    /// Like [`Self::load`], but a summonable vanilla type no feature
    /// implements loads as an entity that only keeps its data.
    pub fn load_or_inert(&self, nbt: &Compound, id: i32) -> Option<Box<dyn RegionEntity>> {
        let name = nbt.get_str("id")?;
        if self.knows(name) {
            return self.load(nbt, id);
        }
        let kind = EntityType::from_name(name)?;
        Some(Box::new(crate::world::inert_entity::Inert::load(
            kind, id, nbt,
        )?))
    }

    /// Lets `kind` spawn naturally, under `rule` besides its placement.
    pub fn register_spawn_rule(&mut self, kind: EntityType, rule: SpawnRule) {
        self.spawn_rules.insert(kind, rule);
    }

    /// The spawn rule of a type that spawns naturally.
    pub fn spawn_rule(&self, kind: EntityType) -> Option<SpawnRule> {
        self.spawn_rules.get(&kind).copied()
    }

    /// Lets `kind` be carried in a bucket, coming out as `release` builds it.
    pub fn register_bucket_release(&mut self, kind: EntityType, release: BucketRelease) {
        self.bucket_releases.insert(kind, release);
    }

    /// The mob of `base`'s type released from a bucket holding `data`;
    /// `None` if no feature lets the type live in a bucket.
    pub fn from_bucket(
        &self,
        base: EntityBase,
        data: Option<Compound>,
        components: Option<&Compound>,
    ) -> Option<Box<dyn RegionEntity>> {
        let release = self.bucket_releases.get(&base.kind)?;
        release(base, data, components)
    }

    /// Lets projectiles of `kind` be launched, built by `launcher`.
    pub fn register_launcher(
        &mut self,
        kind: EntityType,
        launcher: crate::world::projectile::Launcher,
    ) {
        self.launchers.insert(kind, launcher);
    }

    /// The projectile `launch` describes, built by the feature that
    /// implements its type; `None` if none does.
    pub fn launch(
        &self,
        launch: crate::world::projectile::Launch,
        rng: &mut rand::rngs::StdRng,
    ) -> Option<Box<dyn RegionEntity>> {
        let launcher = self.launchers.get(&launch.kind())?;
        launcher(launch, rng)
    }

    /// A new entity of `kind` at `position`, as its loader makes one from
    /// nothing but a position.
    pub fn create(
        &self,
        kind: EntityType,
        id: i32,
        position: [f64; 3],
        yaw: f32,
    ) -> Option<Box<dyn RegionEntity>> {
        let nbt = Compound::new()
            .with("id", kind.name())
            .with("Pos", Tag::List(position.map(Tag::Double).to_vec()))
            .with(
                "Rotation",
                Tag::List(vec![Tag::Float(yaw), Tag::Float(0.0)]),
            );
        self.load(&nbt, id)
    }

    /// Loads a saved entity; `None` if its type is not registered or its
    /// data does not load, in which case it should be kept as raw NBT.
    pub fn load(&self, nbt: &Compound, id: i32) -> Option<Box<dyn RegionEntity>> {
        let (kind, loader) = *self.loaders.get(nbt.get_str("id")?)?;
        let mut rest = nbt.clone();
        rest.remove("id");
        let base = EntityBase::load(kind, id, &mut rest)?;
        let mut entity = loader(base, &mut rest)?;
        entity.base_mut().extra = rest;
        Some(entity)
    }

    /// Splits saved entities into live ones and those kept raw.
    pub fn load_all(
        &self,
        saved: Vec<Compound>,
        mut allocate: impl FnMut() -> i32,
    ) -> (Vec<Box<dyn RegionEntity>>, Vec<Compound>) {
        let mut live = Vec::new();
        let mut raw = Vec::new();
        for nbt in saved {
            let loadable = nbt
                .get_str("id")
                .is_some_and(|id| self.knows(id) || EntityType::from_name(id).is_some());
            match loadable
                .then(|| self.load_or_inert(&nbt, allocate()))
                .flatten()
            {
                Some(entity) => live.push(entity),
                None => raw.push(nbt),
            }
        }
        (live, raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_nbt::Tag;

    #[tuff_core::watchdog]
    #[test]
    fn saved_types_without_a_loader_load_inert_and_unknown_ids_stay_raw() {
        let saved = |id: &str| {
            Compound::new()
                .with("id", id)
                .with("Pos", Tag::List([0.0f64; 3].map(Tag::Double).to_vec()))
        };
        let (live, raw) = Registry::default().load_all(
            vec![saved("minecraft:interaction"), saved("minecraft:nonsense")],
            || 1,
        );
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].base().kind, EntityType::Interaction);
        assert_eq!(raw.len(), 1);
    }
}
