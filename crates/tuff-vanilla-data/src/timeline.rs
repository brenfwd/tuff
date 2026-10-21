//! The built-in `minecraft:timeline` entries: what a world clock's time means.
//! This is a data-driven registry (see [`crate::data_registry`]).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timeline {
    pub name: &'static str,
    /// The `minecraft:world_clock` this timeline follows.
    pub clock: &'static str,
    /// Ticks after which the timeline repeats; `None` if it never does.
    pub period_ticks: Option<u32>,
    /// Sorted by name.
    pub time_markers: &'static [TimeMarker],
    /// `(environment attribute, JSON track)` pairs. A track holds
    /// `keyframes` and optionally `ease` and `modifier`; parse with
    /// [`serde_json`] if you need one.
    pub tracks: &'static [(&'static str, &'static str)],
}

/// A named point in a timeline's period, such as `minecraft:noon`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeMarker {
    pub name: &'static str,
    pub ticks: i32,
    /// Whether `/time set` suggests it.
    pub show_in_commands: bool,
}

impl Timeline {
    pub fn get(name: &str) -> Option<&'static Timeline> {
        crate::lookup(ALL, name)
    }

    /// The value of the environment attribute `attribute` this timeline
    /// sets, at `ticks` on its clock: a number or a flag.
    pub fn sample(&self, attribute: &str, ticks: i64) -> Option<serde_json::Value> {
        let (_, track) = self.tracks.iter().find(|(key, _)| *key == attribute)?;
        sample_track(track, self.period_ticks, ticks)
    }
}

/// Samples a track's JSON as the game's keyframe sampler does.
pub fn sample_track(
    track: &str,
    period_ticks: Option<u32>,
    ticks: i64,
) -> Option<serde_json::Value> {
    use serde_json::Value;
    let json: Value = serde_json::from_str(track).ok()?;
    let mut flags = false;
    let mut keyframes: Vec<(i32, f32)> = Vec::new();
    for frame in json.get("keyframes")?.as_array()? {
        let at = i32::try_from(frame.get("ticks")?.as_i64()?).ok()?;
        let value = match frame.get("value")? {
            Value::Bool(flag) => {
                flags = true;
                f32::from(u8::from(*flag))
            }
            other => other.as_f64()? as f32,
        };
        keyframes.push((at, value));
    }
    if keyframes.is_empty() {
        return None;
    }
    let mut constant = flags;
    let (mut bezier, mut easing) = (None, None);
    match json.get("ease") {
        None => {}
        Some(Value::String(name)) if name == "constant" => constant = true,
        Some(Value::String(name)) if name == "linear" => {}
        Some(Value::String(name)) => easing = Some(crate::easing::Easing::from_name(name)?),
        Some(Value::Object(ease)) => {
            let controls = ease.get("cubic_bezier")?.as_array()?;
            let [x1, y1, x2, y2] = <[f32; 4]>::try_from(
                controls
                    .iter()
                    .map(|v| v.as_f64().map(|v| v as f32))
                    .collect::<Option<Vec<f32>>>()?,
            )
            .ok()?;
            bezier = Some([x1, y1, x2, y2]);
        }
        Some(_) => return None,
    }
    let value =
        crate::world::sample_keyframes(&keyframes, period_ticks, constant, bezier, easing, ticks);
    Some(if flags {
        Value::Bool(value >= 0.5)
    } else {
        serde_json::Number::from_f64(f64::from(value)).map(Value::Number)?
    })
}

/// The most keyframes a pack track may hold.
const MAX_KEYFRAMES: usize = 4096;

impl Timeline {
    /// A `minecraft:timeline` entry from a data pack, checked as the game
    /// checks it. The result lives for the rest of the process.
    pub fn from_json(name: &str, json: &serde_json::Value) -> Result<&'static Timeline, String> {
        use serde_json::Value;
        let map = json.as_object().ok_or("timeline is not an object")?;
        let clock = map
            .get("clock")
            .and_then(Value::as_str)
            .ok_or("timeline has no clock")?;
        let period_ticks = match map.get("period_ticks") {
            None => None,
            Some(period) => Some(
                period
                    .as_u64()
                    .and_then(|p| u32::try_from(p).ok())
                    .filter(|p| *p > 0 && *p <= i32::MAX as u32)
                    .ok_or("period_ticks must be a positive integer")?,
            ),
        };
        let mut time_markers = Vec::new();
        if let Some(markers) = map.get("time_markers") {
            for (marker, info) in markers.as_object().ok_or("time_markers is not an object")? {
                let (ticks, show) = match info {
                    Value::Object(full) => (
                        full.get("ticks"),
                        full.get("show_in_commands")
                            .map_or(Some(false), Value::as_bool)
                            .ok_or("show_in_commands is not a boolean")?,
                    ),
                    ticks => (Some(ticks), false),
                };
                let ticks = ticks
                    .and_then(Value::as_i64)
                    .and_then(|t| i32::try_from(t).ok())
                    .filter(|t| *t >= 0)
                    .ok_or_else(|| format!("time marker {marker} needs non-negative ticks"))?;
                if let Some(period) = period_ticks
                    && ticks as u32 >= period
                {
                    return Err(format!(
                        "Time Marker {marker} must be in range [0; {period})"
                    ));
                }
                time_markers.push(TimeMarker {
                    name: leak(&qualified(marker)),
                    ticks,
                    show_in_commands: show,
                });
            }
        }
        time_markers.sort_by(|a, b| a.name.cmp(b.name));
        let mut tracks = Vec::new();
        if let Some(entries) = map.get("tracks") {
            for (attribute, track) in entries.as_object().ok_or("tracks is not an object")? {
                check_track(track, period_ticks).map_err(|err| format!("{attribute}: {err}"))?;
                tracks.push((leak(&qualified(attribute)), leak(&track.to_string())));
            }
        }
        let timeline = Timeline {
            name: leak(name),
            clock: leak(&qualified(clock)),
            period_ticks,
            time_markers: Box::leak(time_markers.into_boxed_slice()),
            tracks: Box::leak(tracks.into_boxed_slice()),
        };
        crate::world::TimelineTracks::parse(&timeline)?;
        Ok(Box::leak(Box::new(timeline)))
    }
}

fn qualified(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

/// The keyframe rules every track follows whatever its attribute: some
/// keyframes, in tick order, at most two on one tick, within the period.
fn check_track(track: &serde_json::Value, period_ticks: Option<u32>) -> Result<(), String> {
    let keyframes = track
        .get("keyframes")
        .and_then(serde_json::Value::as_array)
        .ok_or("track has no keyframes")?;
    if keyframes.is_empty() {
        return Err("Keyframes must not be empty".into());
    }
    if keyframes.len() > MAX_KEYFRAMES {
        return Err("too many keyframes".into());
    }
    let mut ticks = Vec::with_capacity(keyframes.len());
    for keyframe in keyframes {
        let at = keyframe
            .get("ticks")
            .and_then(serde_json::Value::as_i64)
            .and_then(|t| i32::try_from(t).ok())
            .ok_or("keyframe has no ticks")?;
        if keyframe.get("value").is_none() {
            return Err("keyframe has no value".into());
        }
        ticks.push(at);
    }
    if ticks.windows(2).any(|pair| pair[0] > pair[1]) {
        return Err("Keyframes must be ordered by ticks field".into());
    }
    if ticks
        .windows(3)
        .any(|run| run[0] == run[1] && run[1] == run[2])
    {
        return Err("More than 2 keyframes on same tick".into());
    }
    if let Some(period) = period_ticks
        && let Some(bad) = ticks.iter().find(|t| **t < 0 || **t as u32 > period)
    {
        return Err(format!(
            "Keyframe at tick {bad} must be in range [0; {period}]"
        ));
    }
    Ok(())
}

include!(concat!(env!("OUT_DIR"), "/timelines.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn number(track: &str, period: Option<u32>, ticks: i64) -> f64 {
        sample_track(track, period, ticks)
            .unwrap()
            .as_f64()
            .unwrap()
    }

    const RAMP: &str =
        r#"{"keyframes": [{"ticks": 0, "value": 0.0}, {"ticks": 100, "value": 10.0}]}"#;

    #[test]
    fn tracks_interpolate_and_wrap_from_the_last_keyframe_to_the_first() {
        assert_eq!(number(RAMP, Some(200), 50), 5.0);
        assert_eq!(number(RAMP, Some(200), 150), 5.0);
        assert_eq!(number(RAMP, Some(200), 250), 5.0);
        assert_eq!(number(RAMP, Some(200), -50), 5.0);
        assert_eq!(number(RAMP, None, 500), 10.0);
    }

    #[test]
    fn eases_shape_the_progress_between_keyframes() {
        let quad = r#"{"ease": "in_quad", "keyframes": [{"ticks": 0, "value": 0.0}, {"ticks": 100, "value": 10.0}]}"#;
        assert_eq!(number(quad, None, 50), 2.5);
        let constant = r#"{"ease": "constant", "keyframes": [{"ticks": 0, "value": 1.0}, {"ticks": 100, "value": 10.0}]}"#;
        assert_eq!(number(constant, None, 99), 1.0);
        let bezier = r#"{"ease": {"cubic_bezier": [0.0, 0.0, 1.0, 1.0]}, "keyframes": [{"ticks": 0, "value": 0.0}, {"ticks": 100, "value": 10.0}]}"#;
        assert!((number(bezier, None, 50) - 5.0).abs() < 1.0e-3);
    }

    #[test]
    fn flag_tracks_hold_each_keyframe_until_the_next() {
        let track =
            r#"{"keyframes": [{"ticks": 0, "value": true}, {"ticks": 100, "value": false}]}"#;
        assert_eq!(sample_track(track, Some(200), 99), Some(json!(true)));
        assert_eq!(sample_track(track, Some(200), 100), Some(json!(false)));
        assert_eq!(sample_track(track, Some(200), 199), Some(json!(false)));
        assert_eq!(sample_track(track, Some(200), 200), Some(json!(true)));
    }

    #[test]
    fn a_dimension_reads_the_attributes_its_timelines_set() {
        let overworld = crate::world::Environment::of("minecraft:overworld");
        let key = "minecraft:gameplay/cat_waking_up_gift_chance";
        let at = |ticks: i64| overworld.timeline_attribute(key, |_| ticks);
        assert!(at(100).is_some());
        let nether = crate::world::Environment::of("minecraft:the_nether");
        assert_eq!(nether.timeline_attribute(key, |_| 100), None);
    }
}
