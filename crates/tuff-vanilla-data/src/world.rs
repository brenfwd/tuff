//! Per-dimension gameplay rules taken from each dimension type's
//! environment attributes and timelines, with the game's defaults where a
//! dimension sets none, plus weather's effect on the sky light.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BedCondition {
    Always,
    /// Only while the sky is dark (see [`Environment::is_dark`]).
    WhenDark,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BedRule {
    pub can_sleep: BedCondition,
    pub can_set_spawn: BedCondition,
    pub destroy_on_use: bool,
    pub destroy_on_leave: bool,
    /// Raw JSON text component shown when the bed cannot be slept in.
    pub error_message: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatOp {
    Override,
    Add,
    Subtract,
    Multiply,
    Minimum,
    Maximum,
    /// Moves the subject toward the argument by the track's alpha.
    AlphaBlend,
}

impl FloatOp {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "override" => Self::Override,
            "add" => Self::Add,
            "subtract" => Self::Subtract,
            "multiply" => Self::Multiply,
            "minimum" => Self::Minimum,
            "maximum" => Self::Maximum,
            "alpha_blend" => Self::AlphaBlend,
            _ => return None,
        })
    }

    pub fn apply(self, subject: f32, argument: f32) -> f32 {
        match self {
            Self::Override | Self::AlphaBlend => argument,
            Self::Add => subject + argument,
            Self::Subtract => subject - argument,
            Self::Multiply => subject * argument,
            Self::Minimum => subject.min(argument),
            Self::Maximum => subject.max(argument),
        }
    }
}

/// A timeline track on a number (the sky light level, the surface slime
/// spawn chance), linearly interpolated between keyframes unless
/// `constant`, and repeating every period.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatTrack {
    /// The `minecraft:world_clock` whose time the track follows.
    pub clock: &'static str,
    pub period_ticks: Option<u32>,
    pub op: FloatOp,
    /// Each keyframe's value holds until the next.
    pub constant: bool,
    /// Cubic bezier easing controls `[x1, y1, x2, y2]` between keyframes.
    pub bezier: Option<[f32; 4]>,
    /// A named easing between keyframes, when the track picks one.
    pub easing: Option<crate::easing::Easing>,
    pub keyframes: &'static [(i32, f32)],
    /// The alpha of each keyframe for an alpha blend; empty for other ops.
    pub alphas: &'static [(i32, f32)],
}

impl FloatTrack {
    /// `subject` with this track's argument at `ticks` applied.
    pub fn apply(&self, subject: f32, ticks: i64) -> f32 {
        let argument = self.sample(ticks);
        if self.op != FloatOp::AlphaBlend || self.alphas.is_empty() {
            return self.op.apply(subject, argument);
        }
        let alpha = sample_keyframes(
            self.alphas,
            self.period_ticks,
            self.constant,
            self.bezier,
            self.easing,
            ticks,
        );
        subject + alpha * (argument - subject)
    }

    pub fn sample(&self, ticks: i64) -> f32 {
        sample_keyframes(
            self.keyframes,
            self.period_ticks,
            self.constant,
            self.bezier,
            self.easing,
            ticks,
        )
    }
}

/// The value of a keyframe track at `ticks`: the time loops over `period`,
/// segments run from each keyframe to the next (and from the last to the
/// first, a period later), and the progress through a segment is eased
/// before the values are interpolated.
pub fn sample_keyframes(
    keyframes: &[(i32, f32)],
    period_ticks: Option<u32>,
    constant: bool,
    bezier: Option<[f32; 4]>,
    easing: Option<crate::easing::Easing>,
    ticks: i64,
) -> f32 {
    if keyframes.len() == 1 {
        return keyframes[0].1;
    }
    let ticks = match period_ticks {
        Some(period) => ticks.rem_euclid(period as i64),
        None => ticks,
    };
    let (first, last) = (keyframes[0], keyframes[keyframes.len() - 1]);
    let head = period_ticks.map(|period| {
        (
            (last.0 as i64 - period as i64, last.1),
            (first.0 as i64, first.1),
        )
    });
    let tail = period_ticks.map(|period| {
        (
            (last.0 as i64, last.1),
            (first.0 as i64 + period as i64, first.1),
        )
    });
    let inner = keyframes
        .windows(2)
        .map(|pair| ((pair[0].0 as i64, pair[0].1), (pair[1].0 as i64, pair[1].1)));
    let mut segment = None;
    for candidate in head.into_iter().chain(inner).chain(tail) {
        segment = Some(candidate);
        if ticks < candidate.1.0 {
            break;
        }
    }
    let segment = segment.expect("a track has at least two keyframes here");
    let ((from_ticks, from), (to_ticks, to)) = segment;
    if ticks <= from_ticks {
        return from;
    }
    if ticks >= to_ticks {
        return to;
    }
    if constant {
        return from;
    }
    let alpha = (ticks - from_ticks) as f32 / (to_ticks - from_ticks) as f32;
    let alpha = match (bezier, easing) {
        (Some(controls), _) => cubic_bezier(controls, alpha),
        (None, Some(easing)) => easing.apply(alpha),
        (None, None) => alpha,
    };
    from + alpha * (to - from)
}

/// The coefficients `(a, b, c)` of `a t^3 + b t^2 + c t` for a bezier
/// axis with control values `v1` and `v2`.
fn bezier_curve(v1: f32, v2: f32) -> (f32, f32, f32) {
    (3.0 * v1 - 3.0 * v2 + 1.0, -6.0 * v1 + 3.0 * v2, 3.0 * v1)
}

fn curve_sample((a, b, c): (f32, f32, f32), t: f32) -> f32 {
    ((a * t + b) * t + c) * t
}

fn curve_gradient((a, b, c): (f32, f32, f32), t: f32) -> f32 {
    (3.0 * a * t + 2.0 * b) * t + c
}

/// The eased progress at `x` along a cubic bezier easing, solved for the
/// curve parameter by Newton steps, then bisection, as the game does.
fn cubic_bezier([x1, y1, x2, y2]: [f32; 4], x: f32) -> f32 {
    const EPSILON: f32 = 1.0e-5;
    let x_curve = bezier_curve(x1, x2);
    let y_curve = bezier_curve(y1, y2);
    let mut t = x;
    let mut solved = false;
    for _ in 0..4 {
        let error = curve_sample(x_curve, t) - x;
        if error.abs() < EPSILON {
            solved = true;
            break;
        }
        let gradient = curve_gradient(x_curve, t);
        if gradient < EPSILON {
            break;
        }
        t -= (error / gradient).clamp(-0.25, 0.25);
    }
    if !solved {
        let (mut t0, mut t1) = (0.0f32, 1.0f32);
        while t0 < t1 {
            let error = curve_sample(x_curve, t) - x;
            if error.abs() < EPSILON {
                break;
            }
            if error < 0.0 {
                t0 = t;
            } else {
                t1 = t;
            }
            t = (t1 + t0) / 2.0;
        }
    }
    curve_sample(y_curve, t)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolOp {
    Override,
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
}

impl BoolOp {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "override" => Self::Override,
            "and" => Self::And,
            "nand" => Self::Nand,
            "or" => Self::Or,
            "nor" => Self::Nor,
            "xor" => Self::Xor,
            "xnor" => Self::Xnor,
            _ => return None,
        })
    }

    pub fn apply(self, subject: bool, argument: bool) -> bool {
        match self {
            Self::Override => argument,
            Self::And => subject && argument,
            Self::Nand => !(subject && argument),
            Self::Or => subject || argument,
            Self::Nor => !(subject || argument),
            Self::Xor => subject ^ argument,
            Self::Xnor => subject == argument,
        }
    }
}

/// A timeline track on a flag: each keyframe's value holds until the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlagTrack {
    pub clock: &'static str,
    pub period_ticks: Option<u32>,
    pub op: BoolOp,
    pub keyframes: &'static [(i32, bool)],
}

impl FlagTrack {
    pub fn sample(&self, ticks: i64) -> bool {
        let keyframes = self.keyframes;
        let ticks = match self.period_ticks {
            Some(period) => ticks.rem_euclid(period as i64),
            None => ticks,
        };
        let (first, last) = (keyframes[0], keyframes[keyframes.len() - 1]);
        if ticks < first.0 as i64 {
            return if self.period_ticks.is_some() {
                last.1
            } else {
                first.1
            };
        }
        keyframes
            .iter()
            .rev()
            .find(|(at, _)| ticks >= *at as i64)
            .map_or(first.1, |(_, value)| *value)
    }

    fn apply(&self, subject: bool, ticks: i64) -> bool {
        self.op.apply(subject, self.sample(ticks))
    }
}

/// How weather pulls an attribute toward `value`: by `alpha` at full rain
/// or thunder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeatherBlend {
    pub value: f32,
    pub alpha: f32,
}

impl WeatherBlend {
    fn apply(self, subject: f32, level: f32) -> f32 {
        let target = subject + self.alpha * (self.value - subject);
        subject + level * (target - subject)
    }
}

/// A timeline track on an activity (a villager's schedule): each
/// keyframe's activity holds until the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActivityTrack {
    pub clock: &'static str,
    pub period_ticks: Option<u32>,
    pub keyframes: &'static [(i32, crate::registry::Activity)],
}

impl ActivityTrack {
    pub fn sample(&self, ticks: i64) -> crate::registry::Activity {
        let keyframes = self.keyframes;
        let ticks = match self.period_ticks {
            Some(period) => ticks.rem_euclid(period as i64),
            None => ticks,
        };
        let (first, last) = (keyframes[0], keyframes[keyframes.len() - 1]);
        if ticks < first.0 as i64 {
            return if self.period_ticks.is_some() {
                last.1
            } else {
                first.1
            };
        }
        keyframes
            .iter()
            .rev()
            .find(|(at, _)| ticks >= *at as i64)
            .map_or(first.1, |(_, value)| *value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Environment {
    /// The `minecraft:timeline` entries the dimension type follows.
    pub timelines: &'static [&'static crate::timeline::Timeline],
    pub bed_rule: BedRule,
    pub straw_bed_rule: BedRule,
    /// Water poured from a bucket boils away (the nether).
    pub water_evaporates: bool,
    /// The sky light level before timelines and weather.
    pub sky_light_level: f32,
    /// The sky neither brightens nor darkens.
    pub has_fixed_time: bool,
    pub sky_light_tracks: &'static [FloatTrack],
    /// Whether undead mobs burn in daylight, before timelines.
    pub monsters_burn: bool,
    pub monsters_burn_tracks: &'static [FlagTrack],
    /// The chance slimes spawn on the surface of swamps, before timelines.
    pub surface_slime_spawn_chance: f32,
    pub surface_slime_tracks: &'static [FloatTrack],
    pub turtle_egg_hatch_chance: f32,
    pub turtle_egg_tracks: &'static [FloatTrack],
    /// Piglins and hoglins turn into their zombie kinds here.
    pub piglins_zombify: bool,
    /// A respawn anchor may set the spawn point here; using or respawning
    /// at a charged one where this is false explodes it instead.
    pub respawn_anchor_works: bool,
    /// A Raid Omen may start a raid here.
    pub can_start_raid: bool,
    /// What villagers do, before timelines (their schedule).
    pub villager_activity: crate::registry::Activity,
    pub villager_activity_tracks: &'static [ActivityTrack],
    pub baby_villager_activity: crate::registry::Activity,
    pub baby_villager_activity_tracks: &'static [ActivityTrack],
    /// The angle of the sun in degrees (0 at noon), before timelines.
    pub sun_angle: f32,
    pub sun_angle_tracks: &'static [FloatTrack],
    /// Zombified piglins may spawn from nether portals here.
    pub nether_portal_spawns_piglin: bool,
    /// When eyeblossoms are open; empty tracks leave each as it is.
    pub eyeblossom_open_tracks: &'static [FlagTrack],
    /// Whether creaking hearts are awake without any timeline.
    /// Fire burns out faster here unless the biome says otherwise.
    pub increased_fire_burnout: bool,
    pub creaking_active: bool,
    pub creaking_active_tracks: &'static [FlagTrack],
    /// Whether bees stay in their hives, before timelines and weather.
    pub bees_stay_in_hive: bool,
    pub bees_stay_in_hive_tracks: &'static [FlagTrack],
    /// The natural mob spawns overlaid on every biome's here; `None` leaves
    /// the biomes' as they are.
    pub natural_spawns: Option<&'static crate::biome::NaturalSpawns>,
}

impl Environment {
    /// The attribute `key` as the timelines of this dimension type set it
    /// at the clock times `clock_ticks` gives; the last timeline to set it
    /// wins.
    pub fn timeline_attribute(
        &self,
        key: &str,
        clock_ticks: impl Fn(&str) -> i64,
    ) -> Option<serde_json::Value> {
        self.timelines
            .iter()
            .filter_map(|timeline| timeline.sample(key, clock_ticks(timeline.clock)))
            .last()
    }

    /// The environment of a dimension type, or the defaults for one the
    /// data pack does not define.
    pub fn of(dimension_type: &str) -> &'static Environment {
        ENVIRONMENTS
            .iter()
            .find(|(name, _)| *name == dimension_type)
            .map_or(&DEFAULT_ENVIRONMENT, |(_, environment)| environment)
    }

    /// The sky light level with timelines at `clock_ticks(clock)` and the
    /// given rain and thunder levels (0 to 1).
    pub fn sky_light(&self, clock_ticks: impl Fn(&str) -> i64, rain: f32, thunder: f32) -> f32 {
        let mut level = self.sky_light_level;
        for track in self.sky_light_tracks {
            level = track.apply(level, clock_ticks(track.clock));
        }
        let rain_only = rain - thunder;
        if rain_only > 0.0 {
            level = RAIN_SKY_LIGHT.apply(level, rain_only);
        }
        if thunder > 0.0 {
            level = THUNDER_SKY_LIGHT.apply(level, thunder);
        }
        level.clamp(0.0, 15.0)
    }

    /// The sun's angle in degrees at `clock_ticks(clock)`.
    pub fn sun_angle(&self, clock_ticks: impl Fn(&str) -> i64) -> f32 {
        self.sun_angle_tracks
            .iter()
            .fold(self.sun_angle, |angle, track| {
                track.apply(angle, clock_ticks(track.clock))
            })
    }

    /// Whether undead mobs burn in the sun at `clock_ticks(clock)`.
    pub fn monsters_burn(&self, clock_ticks: impl Fn(&str) -> i64) -> bool {
        self.monsters_burn_tracks
            .iter()
            .fold(self.monsters_burn, |burn, track| {
                track.apply(burn, clock_ticks(track.clock))
            })
    }

    /// Whether eyeblossoms should be open at `clock_ticks(clock)`, or
    /// `None` where the dimension leaves them as they are.
    pub fn eyeblossom_open(&self, clock_ticks: impl Fn(&str) -> i64) -> Option<bool> {
        self.eyeblossom_open_tracks
            .iter()
            .fold(None, |open, track| {
                Some(track.apply(open.unwrap_or(false), clock_ticks(track.clock)))
            })
    }

    /// Whether creaking hearts are awake at `clock_ticks(clock)`.
    pub fn creaking_active(&self, clock_ticks: impl Fn(&str) -> i64) -> bool {
        self.creaking_active_tracks
            .iter()
            .fold(self.creaking_active, |active, track| {
                track.apply(active, clock_ticks(track.clock))
            })
    }

    /// Whether bees stay in their hives at `clock_ticks(clock)`; any rain
    /// or thunder keeps them in too.
    pub fn bees_stay_in_hive(
        &self,
        clock_ticks: impl Fn(&str) -> i64,
        rain: f32,
        thunder: f32,
    ) -> bool {
        let stay = self
            .bees_stay_in_hive_tracks
            .iter()
            .fold(self.bees_stay_in_hive, |stay, track| {
                track.apply(stay, clock_ticks(track.clock))
            });
        stay || rain > 0.0 || thunder > 0.0
    }

    /// The chance a slime may spawn on a swamp's surface at
    /// `clock_ticks(clock)` (it follows the moon in the overworld).
    pub fn surface_slime_spawn_chance(&self, clock_ticks: impl Fn(&str) -> i64) -> f32 {
        self.surface_slime_tracks
            .iter()
            .fold(self.surface_slime_spawn_chance, |chance, track| {
                track.apply(chance, clock_ticks(track.clock))
            })
            .clamp(0.0, 1.0)
    }

    /// The chance a turtle egg's random tick advances its hatching at
    /// `clock_ticks(clock)` (all but certain at night in the overworld).
    pub fn turtle_egg_hatch_chance(&self, clock_ticks: impl Fn(&str) -> i64) -> f32 {
        self.turtle_egg_tracks
            .iter()
            .fold(self.turtle_egg_hatch_chance, |chance, track| {
                track.apply(chance, clock_ticks(track.clock))
            })
            .clamp(0.0, 1.0)
    }

    /// What a villager (or a baby one) should be doing at
    /// `clock_ticks(clock)` by its schedule.
    pub fn villager_activity(
        &self,
        clock_ticks: impl Fn(&str) -> i64,
        baby: bool,
    ) -> crate::registry::Activity {
        let (default, tracks) = if baby {
            (
                self.baby_villager_activity,
                self.baby_villager_activity_tracks,
            )
        } else {
            (self.villager_activity, self.villager_activity_tracks)
        };
        tracks
            .iter()
            .fold(default, |_, track| track.sample(clock_ticks(track.clock)))
    }

    /// Whether it counts as dark outside for sleeping: the sky has darkened
    /// by at least four light levels.
    pub fn is_dark(&self, clock_ticks: impl Fn(&str) -> i64, rain: f32, thunder: f32) -> bool {
        const DARK_SKY_DARKEN: i32 = 4;
        !self.has_fixed_time
            && (15.0 - self.sky_light(clock_ticks, rain, thunder)) as i32 >= DARK_SKY_DARKEN
    }
}

const SKY_LIGHT_LEVEL: &str = "minecraft:gameplay/sky_light_level";
const MONSTERS_BURN: &str = "minecraft:gameplay/monsters_burn";
const SURFACE_SLIME_SPAWN_CHANCE: &str = "minecraft:gameplay/surface_slime_spawn_chance";
const TURTLE_EGG_HATCH_CHANCE: &str = "minecraft:gameplay/turtle_egg_hatch_chance";
const VILLAGER_ACTIVITY: &str = "minecraft:gameplay/villager_activity";
const BABY_VILLAGER_ACTIVITY: &str = "minecraft:gameplay/baby_villager_activity";
const SUN_ANGLE: &str = "minecraft:visual/sun_angle";
const EYEBLOSSOM_OPEN: &str = "minecraft:gameplay/eyeblossom_open";
const CREAKING_ACTIVE: &str = "minecraft:gameplay/creaking_active";
const BEES_STAY_IN_HIVE: &str = "minecraft:gameplay/bees_stay_in_hive";

/// The range an attribute's values must fall in where the game bounds it;
/// a whole timeline or dimension type with a value outside it is invalid,
/// and computed values are clamped into it.
pub fn attribute_range(attribute: &str) -> Option<(f32, f32)> {
    Some(match attribute {
        SKY_LIGHT_LEVEL => (0.0, 15.0),
        SURFACE_SLIME_SPAWN_CHANCE
        | TURTLE_EGG_HATCH_CHANCE
        | "minecraft:gameplay/cat_waking_up_gift_chance"
        | "minecraft:visual/star_brightness"
        | "minecraft:visual/sky_light_factor"
        | "minecraft:audio/music_volume" => (0.0, 1.0),
        "minecraft:gameplay/creature_world_gen_spawn_probability" => (0.0, 0.999_999_9),
        "minecraft:visual/fog_end_distance"
        | "minecraft:visual/sky_fog_end_distance"
        | "minecraft:visual/cloud_fog_end_distance"
        | "minecraft:visual/water_fog_end_distance" => (0.0, f32::INFINITY),
        _ => return None,
    })
}

/// The tracks a timeline holds on the attributes the server models.
#[derive(Debug, Default)]
pub struct TimelineTracks {
    sky_light: Vec<FloatTrack>,
    monsters_burn: Vec<FlagTrack>,
    surface_slime: Vec<FloatTrack>,
    turtle_egg: Vec<FloatTrack>,
    villager_activity: Vec<ActivityTrack>,
    baby_villager_activity: Vec<ActivityTrack>,
    sun_angle: Vec<FloatTrack>,
    eyeblossom_open: Vec<FlagTrack>,
    creaking_active: Vec<FlagTrack>,
    bees_stay_in_hive: Vec<FlagTrack>,
}

fn leak_slice<T>(items: Vec<T>) -> &'static [T] {
    if items.is_empty() {
        return &[];
    }
    Box::leak(items.into_boxed_slice())
}

impl TimelineTracks {
    /// Reads and checks every modelled track of `timeline`.
    pub fn parse(timeline: &crate::timeline::Timeline) -> Result<Self, String> {
        let mut out = Self::default();
        for (attribute, track) in timeline.tracks {
            let json: serde_json::Value =
                serde_json::from_str(track).map_err(|err| format!("{attribute}: {err}"))?;
            let clock = timeline.clock;
            let period = timeline.period_ticks;
            let err = |e: String| format!("{attribute}: {e}");
            match *attribute {
                SKY_LIGHT_LEVEL
                | SURFACE_SLIME_SPAWN_CHANCE
                | TURTLE_EGG_HATCH_CHANCE
                | SUN_ANGLE => {
                    let parsed = float_track(clock, period, &json, attribute_range(attribute))
                        .map_err(err)?;
                    match *attribute {
                        SKY_LIGHT_LEVEL => out.sky_light.push(parsed),
                        SURFACE_SLIME_SPAWN_CHANCE => out.surface_slime.push(parsed),
                        TURTLE_EGG_HATCH_CHANCE => out.turtle_egg.push(parsed),
                        _ => out.sun_angle.push(parsed),
                    }
                }
                MONSTERS_BURN | EYEBLOSSOM_OPEN | CREAKING_ACTIVE | BEES_STAY_IN_HIVE => {
                    let override_only = *attribute == EYEBLOSSOM_OPEN;
                    let parsed = flag_track(clock, period, &json, override_only).map_err(err)?;
                    match *attribute {
                        MONSTERS_BURN => out.monsters_burn.push(parsed),
                        EYEBLOSSOM_OPEN => out.eyeblossom_open.push(parsed),
                        CREAKING_ACTIVE => out.creaking_active.push(parsed),
                        _ => out.bees_stay_in_hive.push(parsed),
                    }
                }
                VILLAGER_ACTIVITY | BABY_VILLAGER_ACTIVITY => {
                    let parsed = activity_track(clock, period, &json).map_err(err)?;
                    if *attribute == VILLAGER_ACTIVITY {
                        out.villager_activity.push(parsed);
                    } else {
                        out.baby_villager_activity.push(parsed);
                    }
                }
                other => {
                    if let Some(range) = attribute_range(other) {
                        float_track(clock, period, &json, Some(range)).map_err(err)?;
                    }
                }
            }
        }
        Ok(out)
    }

    fn extend(&mut self, other: Self) {
        self.sky_light.extend(other.sky_light);
        self.monsters_burn.extend(other.monsters_burn);
        self.surface_slime.extend(other.surface_slime);
        self.turtle_egg.extend(other.turtle_egg);
        self.villager_activity.extend(other.villager_activity);
        self.baby_villager_activity
            .extend(other.baby_villager_activity);
        self.sun_angle.extend(other.sun_angle);
        self.eyeblossom_open.extend(other.eyeblossom_open);
        self.creaking_active.extend(other.creaking_active);
        self.bees_stay_in_hive.extend(other.bees_stay_in_hive);
    }
}

fn keyframes(track: &serde_json::Value) -> Result<&Vec<serde_json::Value>, String> {
    track
        .get("keyframes")
        .and_then(serde_json::Value::as_array)
        .filter(|frames| !frames.is_empty())
        .ok_or_else(|| "track has no keyframes".to_owned())
}

fn keyframe_ticks(frame: &serde_json::Value) -> Result<i32, String> {
    frame
        .get("ticks")
        .and_then(serde_json::Value::as_i64)
        .and_then(|t| i32::try_from(t).ok())
        .ok_or_else(|| "bad keyframe ticks".to_owned())
}

fn modifier(track: &serde_json::Value) -> Result<&str, String> {
    match track.get("modifier") {
        None => Ok("override"),
        Some(name) => name
            .as_str()
            .ok_or_else(|| "modifier is not a string".to_owned()),
    }
}

fn float_track(
    clock: &'static str,
    period_ticks: Option<u32>,
    track: &serde_json::Value,
    range: Option<(f32, f32)>,
) -> Result<FloatTrack, String> {
    use serde_json::Value;
    let name = modifier(track)?;
    let op = FloatOp::from_name(name).ok_or_else(|| format!("unknown float modifier {name}"))?;
    let (mut constant, mut bezier, mut easing) = (false, None, None);
    match track.get("ease") {
        None => {}
        Some(Value::String(name)) if name == "constant" => constant = true,
        Some(Value::String(name)) if name == "linear" => {}
        Some(Value::String(name)) => {
            easing = Some(
                crate::easing::Easing::from_name(name)
                    .ok_or_else(|| format!("unknown easing {name}"))?,
            )
        }
        Some(Value::Object(ease)) => {
            let controls: Vec<f32> = ease
                .get("cubic_bezier")
                .and_then(Value::as_array)
                .ok_or("bad easing")?
                .iter()
                .map(|v| v.as_f64().map(|v| v as f32))
                .collect::<Option<_>>()
                .ok_or("bad cubic_bezier")?;
            let controls = <[f32; 4]>::try_from(controls).map_err(|_| "bad cubic_bezier")?;
            if !(0.0..=1.0).contains(&controls[0]) || !(0.0..=1.0).contains(&controls[2]) {
                return Err("cubic_bezier x controls must be within [0; 1]".into());
            }
            bezier = Some(controls);
        }
        Some(_) => return Err("bad easing".into()),
    }
    let mut values = Vec::new();
    let mut alphas = Vec::new();
    for frame in keyframes(track)? {
        let ticks = keyframe_ticks(frame)?;
        let raw = frame.get("value").ok_or("keyframe has no value")?;
        let value = match (op, raw) {
            (FloatOp::AlphaBlend, Value::Object(blend)) => {
                let alpha = match blend.get("alpha") {
                    None => 1.0,
                    Some(alpha) => alpha.as_f64().ok_or("alpha is not a number")? as f32,
                };
                if !(0.0..=1.0).contains(&alpha) {
                    return Err(format!("alpha {alpha} is not in range [0; 1]"));
                }
                alphas.push((ticks, alpha));
                blend.get("value").and_then(Value::as_f64)
            }
            (FloatOp::AlphaBlend, raw) => {
                alphas.push((ticks, 1.0));
                raw.as_f64()
            }
            (_, raw) => raw.as_f64(),
        };
        let value = value.ok_or("keyframe value is not a number")? as f32;
        if op == FloatOp::Override
            && let Some((min, max)) = range
            && !(min..=max).contains(&value)
        {
            return Err(format!("{value} is not in range [{min}; {max}]"));
        }
        values.push((ticks, value));
    }
    Ok(FloatTrack {
        clock,
        period_ticks,
        op,
        constant,
        bezier,
        easing,
        keyframes: leak_slice(values),
        alphas: leak_slice(alphas),
    })
}

fn flag_track(
    clock: &'static str,
    period_ticks: Option<u32>,
    track: &serde_json::Value,
    override_only: bool,
) -> Result<FlagTrack, String> {
    let name = modifier(track)?;
    let op = BoolOp::from_name(name)
        .filter(|op| !override_only || *op == BoolOp::Override)
        .ok_or_else(|| format!("unknown boolean modifier {name}"))?;
    let mut values = Vec::new();
    for frame in keyframes(track)? {
        let value = frame
            .get("value")
            .and_then(serde_json::Value::as_bool)
            .ok_or("keyframe value is not a boolean")?;
        values.push((keyframe_ticks(frame)?, value));
    }
    Ok(FlagTrack {
        clock,
        period_ticks,
        op,
        keyframes: leak_slice(values),
    })
}

fn activity_track(
    clock: &'static str,
    period_ticks: Option<u32>,
    track: &serde_json::Value,
) -> Result<ActivityTrack, String> {
    let name = modifier(track)?;
    if name != "override" {
        return Err(format!("unknown activity modifier {name}"));
    }
    let mut values = Vec::new();
    for frame in keyframes(track)? {
        let name = frame
            .get("value")
            .and_then(serde_json::Value::as_str)
            .ok_or("keyframe value is not an activity")?;
        let full = if name.contains(':') {
            name.to_owned()
        } else {
            format!("minecraft:{name}")
        };
        let activity = crate::registry::Activity::from_name(&full)
            .ok_or_else(|| format!("unknown activity {name}"))?;
        values.push((keyframe_ticks(frame)?, activity));
    }
    Ok(ActivityTrack {
        clock,
        period_ticks,
        keyframes: leak_slice(values),
    })
}

impl Environment {
    /// This environment following `timelines` instead of its own: the
    /// tracks are read from the timelines, in order.
    pub fn with_timelines(
        &self,
        timelines: &'static [&'static crate::timeline::Timeline],
    ) -> Result<Environment, String> {
        let mut tracks = TimelineTracks::default();
        for timeline in timelines {
            tracks.extend(
                TimelineTracks::parse(timeline)
                    .map_err(|err| format!("timeline {}: {err}", timeline.name))?,
            );
        }
        Ok(Environment {
            timelines,
            sky_light_tracks: leak_slice(tracks.sky_light),
            monsters_burn_tracks: leak_slice(tracks.monsters_burn),
            surface_slime_tracks: leak_slice(tracks.surface_slime),
            turtle_egg_tracks: leak_slice(tracks.turtle_egg),
            villager_activity_tracks: leak_slice(tracks.villager_activity),
            baby_villager_activity_tracks: leak_slice(tracks.baby_villager_activity),
            sun_angle_tracks: leak_slice(tracks.sun_angle),
            eyeblossom_open_tracks: leak_slice(tracks.eyeblossom_open),
            creaking_active_tracks: leak_slice(tracks.creaking_active),
            bees_stay_in_hive_tracks: leak_slice(tracks.bees_stay_in_hive),
            ..*self
        })
    }
}

include!(concat!(env!("OUT_DIR"), "/world.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boolean_modifiers_follow_their_truth_tables() {
        let table = |op: BoolOp| {
            [(false, false), (false, true), (true, false), (true, true)]
                .map(|(subject, argument)| op.apply(subject, argument))
        };
        assert_eq!(table(BoolOp::Override), [false, true, false, true]);
        assert_eq!(table(BoolOp::And), [false, false, false, true]);
        assert_eq!(table(BoolOp::Nand), [true, true, true, false]);
        assert_eq!(table(BoolOp::Or), [false, true, true, true]);
        assert_eq!(table(BoolOp::Nor), [true, false, false, false]);
        assert_eq!(table(BoolOp::Xor), [false, true, true, false]);
        assert_eq!(table(BoolOp::Xnor), [true, false, false, true]);
    }

    #[test]
    fn overworld_nights_and_thunderstorms_are_dark() {
        let overworld = Environment::of("minecraft:overworld");
        let at = |ticks: i64| move |_: &str| ticks;
        assert!(!overworld.is_dark(at(6000), 0.0, 0.0));
        assert!(overworld.is_dark(at(18000), 0.0, 0.0));
        assert!(
            !overworld.is_dark(at(6000), 1.0, 0.0),
            "rain alone is not dark"
        );
        assert!(overworld.is_dark(at(6000), 1.0, 1.0), "thunder is");
        assert!(!Environment::of("minecraft:the_nether").is_dark(at(18000), 0.0, 0.0));
    }

    #[test]
    fn the_overworld_sun_is_overhead_at_noon_and_underfoot_at_midnight() {
        let overworld = Environment::of("minecraft:overworld");
        let at = |ticks: i64| move |_: &str| ticks;
        assert_eq!(overworld.sun_angle(at(6000)), 0.0);
        assert!((overworld.sun_angle(at(18000)) - 180.0).abs() < 0.01);
        let dawn = overworld.sun_angle(at(0));
        assert!(
            dawn > 270.0 && dawn < 360.0,
            "eased past a quarter turn: {dawn}"
        );
    }

    #[test]
    fn monsters_burn_during_the_overworld_day_only() {
        let overworld = Environment::of("minecraft:overworld");
        let at = |ticks: i64| move |_: &str| ticks;
        assert!(overworld.monsters_burn(at(6000)));
        assert!(overworld.monsters_burn(at(24000 + 1000)));
        assert!(overworld.monsters_burn(at(23460)));
        assert!(!overworld.monsters_burn(at(12542)));
        assert!(!overworld.monsters_burn(at(18000)));
        assert!(!Environment::of("minecraft:the_nether").monsters_burn(at(6000)));
    }

    #[test]
    fn bees_stay_in_at_overworld_night_and_in_rain() {
        let overworld = Environment::of("minecraft:overworld");
        let at = |ticks: i64| move |_: &str| ticks;
        assert!(!overworld.bees_stay_in_hive(at(6000), 0.0, 0.0));
        assert!(overworld.bees_stay_in_hive(at(6000), 0.1, 0.0));
        assert!(overworld.bees_stay_in_hive(at(12542), 0.0, 0.0));
        assert!(overworld.bees_stay_in_hive(at(18000), 0.0, 0.0));
        assert!(!overworld.bees_stay_in_hive(at(23460), 0.0, 0.0));
        let nether = Environment::of("minecraft:the_nether");
        assert!(!nether.bees_stay_in_hive(at(18000), 0.0, 0.0));
    }

    #[test]
    fn creaking_hearts_wake_at_overworld_night_only() {
        let overworld = Environment::of("minecraft:overworld");
        let at = |ticks: i64| move |_: &str| ticks;
        assert!(!overworld.creaking_active(at(6000)));
        assert!(overworld.creaking_active(at(12600)));
        assert!(overworld.creaking_active(at(18000)));
        assert!(!overworld.creaking_active(at(23401)));
        assert!(!Environment::of("minecraft:the_nether").creaking_active(at(18000)));
    }

    #[test]
    fn turtle_eggs_hatch_by_night() {
        let overworld = Environment::of("minecraft:overworld");
        let at = |ticks: i64| move |_: &str| ticks;
        assert_eq!(overworld.turtle_egg_hatch_chance(at(6000)), 0.002);
        assert_eq!(overworld.turtle_egg_hatch_chance(at(21500)), 1.0);
        let nether = Environment::of("minecraft:the_nether");
        assert_eq!(nether.turtle_egg_hatch_chance(at(21500)), 0.002);
    }

    #[test]
    fn surface_slimes_follow_the_moon() {
        let overworld = Environment::of("minecraft:overworld");
        let at = |ticks: i64| move |_: &str| ticks;
        assert_eq!(overworld.surface_slime_spawn_chance(at(0)), 0.5);
        assert_eq!(overworld.surface_slime_spawn_chance(at(23999)), 0.5);
        assert_eq!(overworld.surface_slime_spawn_chance(at(24000)), 0.375);
        assert_eq!(overworld.surface_slime_spawn_chance(at(96000 + 5)), 0.0);
        assert_eq!(
            overworld.surface_slime_spawn_chance(at(192000 + 30000)),
            0.375
        );
        let nether = Environment::of("minecraft:the_nether");
        assert_eq!(nether.surface_slime_spawn_chance(at(0)), 0.0);
        assert!(overworld.piglins_zombify && !nether.piglins_zombify);
        assert!(overworld.can_start_raid && !nether.can_start_raid);
        assert!(Environment::of("minecraft:the_end").piglins_zombify);
    }

    #[test]
    fn bed_rules_follow_the_dimension() {
        let overworld = Environment::of("minecraft:overworld");
        assert_eq!(overworld.bed_rule.can_sleep, BedCondition::WhenDark);
        assert_eq!(overworld.bed_rule.can_set_spawn, BedCondition::Always);
        assert!(overworld.straw_bed_rule.destroy_on_leave);
        let nether = Environment::of("minecraft:the_nether");
        assert!(nether.bed_rule.destroy_on_use);
        assert!(nether.water_evaporates);
        assert!(!overworld.water_evaporates);
        assert_eq!(WAKE_UP_MARKER, "minecraft:wake_up_from_sleep");
    }

    fn timeline(json: serde_json::Value) -> Result<&'static crate::timeline::Timeline, String> {
        crate::timeline::Timeline::from_json("test:timeline", &json)
    }

    #[test]
    fn runtime_timelines_rebuild_every_vanilla_environment() {
        for (name, environment) in ENVIRONMENTS {
            let rebuilt = environment.with_timelines(environment.timelines).unwrap();
            assert_eq!(&rebuilt, environment, "{name}");
        }
        for (name, timeline) in crate::timeline::ALL {
            TimelineTracks::parse(timeline).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn alpha_blend_moves_toward_the_argument_by_the_interpolated_alpha() {
        let custom = timeline(serde_json::json!({
            "clock": "minecraft:overworld",
            "period_ticks": 100,
            "tracks": {"minecraft:gameplay/sky_light_level": {
                "modifier": "alpha_blend",
                "keyframes": [
                    {"ticks": 0, "value": {"value": 0.0, "alpha": 0.0}},
                    {"ticks": 50, "value": {"value": 0.0, "alpha": 0.5}},
                    {"ticks": 60, "value": 4.0}
                ]
            }}
        }))
        .unwrap();
        let timelines: &'static [_] = Box::leak(Box::new([custom]));
        let environment = DEFAULT_ENVIRONMENT.with_timelines(timelines).unwrap();
        assert_eq!(environment.sky_light(|_| 0, 0.0, 0.0), 15.0);
        assert_eq!(environment.sky_light(|_| 25, 0.0, 0.0), 15.0 - 0.25 * 15.0);
        assert_eq!(environment.sky_light(|_| 50, 0.0, 0.0), 7.5);
        assert_eq!(environment.sky_light(|_| 60, 0.0, 0.0), 4.0);
        assert_eq!(FloatOp::from_name("alpha_blend"), Some(FloatOp::AlphaBlend));
    }

    #[test]
    fn custom_boolean_tracks_combine_with_the_base_value() {
        let custom = timeline(serde_json::json!({
            "clock": "minecraft:overworld",
            "tracks": {"minecraft:gameplay/monsters_burn": {
                "modifier": "xnor",
                "keyframes": [{"ticks": 0, "value": false}, {"ticks": 10, "value": true}]
            }}
        }))
        .unwrap();
        let timelines: &'static [_] = Box::leak(Box::new([custom]));
        let environment = DEFAULT_ENVIRONMENT.with_timelines(timelines).unwrap();
        let base = DEFAULT_ENVIRONMENT.monsters_burn;
        assert_eq!(environment.monsters_burn(|_| 0), !base);
        assert_eq!(environment.monsters_burn(|_| 10), base);
    }

    #[test]
    fn invalid_timelines_are_rejected() {
        let track = |attribute: &str, track: serde_json::Value| {
            timeline(serde_json::json!({
                "clock": "minecraft:overworld",
                "period_ticks": 100,
                "tracks": {attribute: track}
            }))
        };
        let light = "minecraft:gameplay/sky_light_level";
        let frames = |v: serde_json::Value| serde_json::json!({"keyframes": v});
        assert!(
            track(
                light,
                frames(serde_json::json!([{"ticks": 0, "value": 16.0}]))
            )
            .is_err()
        );
        assert!(
            track(
                light,
                serde_json::json!({"modifier": "add", "keyframes": [{"ticks": 0, "value": 16.0}]})
            )
            .is_ok()
        );
        assert!(
            track(
                light,
                frames(serde_json::json!([{"ticks": 101, "value": 1.0}]))
            )
            .is_err()
        );
        assert!(
            track(
                light,
                frames(serde_json::json!([{"ticks": 5, "value": 1.0}, {"ticks": 4, "value": 1.0}]))
            )
            .is_err()
        );
        assert!(
            track(
                light,
                frames(serde_json::json!([
                    {"ticks": 5, "value": 1.0}, {"ticks": 5, "value": 1.0}, {"ticks": 5, "value": 1.0}
                ]))
            )
            .is_err()
        );
        assert!(track(light, frames(serde_json::json!([]))).is_err());
        assert!(
            track(
                light,
                serde_json::json!({"modifier": "and", "keyframes": [{"ticks": 0, "value": 1.0}]})
            )
            .is_err()
        );
        assert!(
            track(
                light,
                serde_json::json!({"modifier": "alpha_blend",
                    "keyframes": [{"ticks": 0, "value": {"value": 1.0, "alpha": 2.0}}]})
            )
            .is_err()
        );
        assert!(
            track(
                "minecraft:gameplay/eyeblossom_open",
                serde_json::json!({"modifier": "or", "keyframes": [{"ticks": 0, "value": true}]})
            )
            .is_err()
        );
        assert!(
            track(
                "minecraft:gameplay/villager_activity",
                frames(serde_json::json!([{"ticks": 0, "value": "minecraft:nonsense"}]))
            )
            .is_err()
        );
        assert!(
            timeline(serde_json::json!({
                "clock": "minecraft:overworld",
                "period_ticks": 100,
                "time_markers": {"test:late": 100}
            }))
            .is_err()
        );
        assert!(
            timeline(serde_json::json!({"clock": "minecraft:overworld", "period_ticks": 0}))
                .is_err()
        );
    }
}
