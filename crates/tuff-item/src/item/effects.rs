use super::*;

impl Enchantments {
    pub fn new(entries: impl IntoIterator<Item = (String, i32)>) -> Self {
        let mut entries: Vec<(String, i32)> = entries.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        Self(entries)
    }

    pub fn entries(&self) -> &[(String, i32)] {
        &self.0
    }

    pub fn level(&self, id: &str) -> i32 {
        self.0
            .binary_search_by(|(entry, _)| entry.as_str().cmp(id))
            .map_or(0, |i| self.0[i].1)
    }

    /// The registry index of each enchantment the client knows, with its
    /// level.
    pub fn network_entries(&self) -> impl Iterator<Item = (u32, i32)> + '_ {
        let registry = crate::enchantment::current();
        self.0
            .iter()
            .filter_map(move |(id, level)| Some((registry.index_of(id)?, *level)))
    }

    pub fn from_network_index(index: u32) -> Option<String> {
        crate::enchantment::current().id(index).map(str::to_owned)
    }

    pub(super) fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        let map = match map.get("levels") {
            Some(levels) => levels.as_compound()?,
            None => map,
        };
        let mut entries = Vec::with_capacity(map.len());
        for (id, level) in map.iter() {
            let level = as_i64(level).filter(|l| (1..=MAX_ENCHANTMENT_LEVEL).contains(l))?;
            entries.push((id.to_owned(), level as i32));
        }
        Some(Self::new(entries))
    }

    pub(super) fn to_nbt(&self) -> Compound {
        self.0
            .iter()
            .map(|(id, level)| (id.clone(), Tag::Int(*level)))
            .collect()
    }

    /// Only entries the client received (those in its registry) take part.
    pub(super) fn canon(&self) -> Canon {
        Canon::Map(
            self.0
                .iter()
                .filter(|(id, _)| crate::enchantment::current().contains(id))
                .map(|(id, level)| (id.clone(), Canon::Int(*level)))
                .collect(),
        )
    }
}

impl PotionContents {
    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        if let Some(potion) = tag.as_str() {
            return Some(Self {
                potion: Some(Potion::from_name(potion)?),
                ..Default::default()
            });
        }
        let map = tag.as_compound()?;
        let potion = match map.get("potion") {
            Some(potion) => Some(Potion::from_name(potion.as_str()?)?),
            None => None,
        };
        let custom_color = match map.get("custom_color") {
            Some(color) => Some(as_i64(color)? as i32),
            None => None,
        };
        let custom_effects = match map.get("custom_effects") {
            Some(effects) => effects
                .as_list()?
                .iter()
                .map(PotionEffect::from_nbt)
                .collect::<Option<Vec<_>>>()?,
            None => Vec::new(),
        };
        let custom_name = match map.get("custom_name") {
            Some(name) => Some(name.as_str()?.to_owned()),
            None => None,
        };
        Some(Self {
            potion,
            custom_color,
            custom_effects,
            custom_name,
        })
    }

    pub fn to_nbt(&self) -> Compound {
        let mut nbt = Compound::new();
        if let Some(potion) = self.potion {
            nbt.insert("potion", potion.name());
        }
        if let Some(color) = self.custom_color {
            nbt.insert("custom_color", color);
        }
        if !self.custom_effects.is_empty() {
            nbt.insert(
                "custom_effects",
                Tag::List(
                    self.custom_effects
                        .iter()
                        .map(|effect| Tag::Compound(effect.to_nbt()))
                        .collect(),
                ),
            );
        }
        if let Some(name) = &self.custom_name {
            nbt.insert("custom_name", name.as_str());
        }
        nbt
    }

    pub(super) fn canon(&self) -> Canon {
        let mut entries = Vec::new();
        if let Some(potion) = self.potion {
            entries.push(("potion".to_owned(), Canon::str(potion.name())));
        }
        if let Some(color) = self.custom_color {
            entries.push(("custom_color".to_owned(), Canon::Int(color)));
        }
        if !self.custom_effects.is_empty() {
            entries.push((
                "custom_effects".to_owned(),
                Canon::List(
                    self.custom_effects
                        .iter()
                        .map(PotionEffect::canon)
                        .collect(),
                ),
            ));
        }
        if let Some(name) = &self.custom_name {
            entries.push(("custom_name".to_owned(), Canon::str(name.clone())));
        }
        Canon::Map(entries)
    }
}

impl PotionEffect {
    pub(super) fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        Some(Self {
            effect: MobEffect::from_name(map.get_str("id")?)?,
            details: EffectDetails::from_nbt(map)?,
        })
    }

    pub(super) fn to_nbt(&self) -> Compound {
        let mut nbt = Compound::new().with("id", self.effect.name());
        self.details.write_nbt(&mut nbt);
        nbt
    }

    pub(super) fn canon(&self) -> Canon {
        let mut entries = vec![("id".to_owned(), Canon::str(self.effect.name()))];
        self.details.canon_entries(&mut entries);
        Canon::Map(entries)
    }
}

impl Default for EffectDetails {
    fn default() -> Self {
        Self {
            amplifier: 0,
            duration: 0,
            ambient: false,
            show_particles: true,
            show_icon: true,
            hidden_effect: None,
        }
    }
}

impl EffectDetails {
    pub(super) fn from_nbt(map: &Compound) -> Option<Self> {
        let amplifier = match map.get("amplifier") {
            Some(amplifier) => u8::try_from(as_i64(amplifier)?).ok()?,
            None => 0,
        };
        let duration = match map.get("duration") {
            Some(duration) => as_i64(duration)? as i32,
            None => 0,
        };
        let ambient = match map.get("ambient") {
            Some(ambient) => as_bool(ambient)?,
            None => false,
        };
        let show_particles = match map.get("show_particles") {
            Some(show) => as_bool(show)?,
            None => true,
        };
        let show_icon = match map.get("show_icon") {
            Some(show) => as_bool(show)?,
            None => show_particles,
        };
        let hidden_effect = match map.get("hidden_effect") {
            Some(hidden) => Some(Box::new(Self::from_nbt(hidden.as_compound()?)?)),
            None => None,
        };
        Some(Self {
            amplifier,
            duration,
            ambient,
            show_particles,
            show_icon,
            hidden_effect,
        })
    }

    pub(super) fn write_nbt(&self, nbt: &mut Compound) {
        if self.amplifier != 0 {
            nbt.insert("amplifier", self.amplifier as i8);
        }
        if self.duration != 0 {
            nbt.insert("duration", self.duration);
        }
        if self.ambient {
            nbt.insert("ambient", true);
        }
        if !self.show_particles {
            nbt.insert("show_particles", false);
        }
        nbt.insert("show_icon", self.show_icon);
        if let Some(hidden) = &self.hidden_effect {
            let mut inner = Compound::new();
            hidden.write_nbt(&mut inner);
            nbt.insert("hidden_effect", inner);
        }
    }

    pub(super) fn canon_entries(&self, entries: &mut Vec<(String, Canon)>) {
        if self.amplifier != 0 {
            entries.push(("amplifier".to_owned(), Canon::Byte(self.amplifier as i8)));
        }
        if self.duration != 0 {
            entries.push(("duration".to_owned(), Canon::Int(self.duration)));
        }
        if self.ambient {
            entries.push(("ambient".to_owned(), Canon::Bool(true)));
        }
        if !self.show_particles {
            entries.push(("show_particles".to_owned(), Canon::Bool(false)));
        }
        entries.push(("show_icon".to_owned(), Canon::Bool(self.show_icon)));
        if let Some(hidden) = &self.hidden_effect {
            let mut inner = Vec::new();
            hidden.canon_entries(&mut inner);
            entries.push(("hidden_effect".to_owned(), Canon::Map(inner)));
        }
    }
}

/// How many of `amount` independent points Unbreaking removes, each with
/// probability `chance`. Rolling each point individually is exact but, for
/// a lot of points, wasteful; above 128 points -- and only once the
/// binomial distribution is well clear of its edges (expected drops and
/// expected survivors both at least 20) -- this switches to a normal
/// approximation of the same distribution: mean `floor(n * chance)`,
/// variance `n * chance * (1 - chance)`, rounded to the nearest integer and
/// clamped to `[0, n]`.
pub(crate) fn dropped_points(amount: u32, chance: f32, rng: &mut impl Rng) -> u32 {
    let n = amount as f32;
    let expected_drops = n * chance;
    let expected_survivors = n * (1.0 - chance);
    if n <= 128.0 || expected_drops < 20.0 || expected_survivors < 20.0 {
        return (0..amount).filter(|_| rng.r#gen::<f32>() < chance).count() as u32;
    }
    let mean = expected_drops.floor() as f64;
    let variance = (n * chance * (1.0 - chance)) as f64;
    let sigma = variance.sqrt();
    let sample = mean + standard_normal(rng) * sigma;
    sample.round().clamp(0.0, n as f64) as u32
}

/// A standard normal (mean 0, variance 1) sample via the Box-Muller
/// transform, using two independent uniform samples in (0, 1].
pub(super) fn standard_normal(rng: &mut impl Rng) -> f64 {
    let u1: f64 = 1.0 - rng.r#gen::<f64>();
    let u2: f64 = rng.r#gen::<f64>();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}
