//! Decoding helpers and deep copies of the generated worldgen shapes.

use super::*;

pub(super) fn decode_noise(id: &str, value: &Value) -> Result<Noise> {
    let o = obj(
        value,
        &[
            "base_amplitude",
            "base_octave",
            "octave_count",
            "normalize",
            "amplitude_modifiers",
        ],
        "noise",
    )?;
    let octave_count = o.i32_or("octave_count", 1)?;
    if !(1..=32).contains(&octave_count) {
        return Err("octave count out of range".into());
    }
    let base_octave = o.i32("base_octave")?;
    if !(-32..=32).contains(&base_octave) {
        return Err("base octave out of range".into());
    }
    let normalize = match o.get("normalize") {
        None | Some(Value::Bool(true)) => Normalize::Enabled,
        Some(Value::Bool(false)) => Normalize::Disabled,
        Some(Value::String(s)) if s == "legacy" => Normalize::Legacy,
        Some(other) => return Err(format!("bad normalization {other}")),
    };
    let modifiers: Vec<f64> = match o.get("amplitude_modifiers") {
        None => Vec::new(),
        Some(_) => o
            .list("amplitude_modifiers")?
            .iter()
            .map(|v| v.as_f64().ok_or("amplitudes are numbers"))
            .collect::<std::result::Result<_, _>>()?,
    };
    if !modifiers.is_empty() && modifiers.len() != octave_count as usize {
        return Err("amplitude modifiers must match the octave count".into());
    }
    Ok(Noise {
        name: Box::leak(id.to_owned().into_boxed_str()),
        base_octave,
        base_amplitude: o.f64_or("base_amplitude", 1.0)?,
        octave_count,
        normalize,
        amplitude_modifiers: modifiers.leak(),
    })
}

fn quantize(v: f64) -> i64 {
    ((v as f32) * 10000.0f32) as i64
}

pub(super) fn climate_parameter(value: &Value) -> Result<ClimateParameter> {
    let (min, max) = match value {
        Value::Number(n) => {
            let v = n.as_f64().ok_or("bad number")?;
            (v, v)
        }
        Value::Array(pair) if pair.len() == 2 => (
            pair[0].as_f64().ok_or("bad min")?,
            pair[1].as_f64().ok_or("bad max")?,
        ),
        Value::Object(_) => {
            let o = obj(value, &["min", "max"], "climate range")?;
            (o.f64("min")?, o.f64("max")?)
        }
        _ => return Err(format!("bad climate parameter {value}")),
    };
    if min as f32 > max as f32 {
        return Err("climate range min > max".into());
    }
    Ok(ClimateParameter {
        min: quantize(min),
        max: quantize(max),
    })
}

pub(super) fn biomes(value: &Value) -> Result<&'static [&'static str]> {
    let names: Vec<&'static str> = match value {
        Value::String(name) if name.starts_with('#') => {
            let tag = qualify(&name[1..]);
            return tuff_vanilla_data::tag::worldgen::biome::ALL
                .iter()
                .find(|(name, _)| *name == tag)
                .map(|(_, members)| *members)
                .ok_or_else(|| format!("unknown biome tag {tag}"));
        }
        Value::String(name) => vec![biome(name)?],
        Value::Array(list) => list
            .iter()
            .map(|v| biome(v.as_str().ok_or("biomes are named")?))
            .collect::<Result<_>>()?,
        _ => return Err(format!("bad biome set {value}")),
    };
    Ok(names.leak())
}

fn biome(name: &str) -> Result<&'static str> {
    let name = qualify(name);
    tuff_vanilla_data::data_registry::WORLDGEN_BIOME
        .entries
        .iter()
        .find(|entry| **entry == name)
        .copied()
        .ok_or_else(|| format!("unknown biome {name}"))
}

pub(super) fn clone_df(f: &DensityFunction) -> DensityFunction {
    use DensityFunction as D;
    match f {
        D::Constant(v) => D::Constant(*v),
        D::Reference(n) => D::Reference(n),
        D::Noise {
            noise,
            xz_scale,
            y_scale,
            shift_x,
            shift_y,
            shift_z,
        } => D::Noise {
            noise,
            xz_scale: *xz_scale,
            y_scale: *y_scale,
            shift_x,
            shift_y,
            shift_z,
        },
        D::ShiftA(n) => D::ShiftA(n),
        D::ShiftB(n) => D::ShiftB(n),
        D::Shift(n) => D::Shift(n),
        D::EndIslands => D::EndIslands,
        D::DistanceToPoint { point, metric } => D::DistanceToPoint {
            point: *point,
            metric: *metric,
        },
        D::Gradient {
            axis,
            tiling,
            from_coordinate,
            to_coordinate,
            from_value,
            to_value,
        } => D::Gradient {
            axis: *axis,
            tiling: *tiling,
            from_coordinate: *from_coordinate,
            to_coordinate: *to_coordinate,
            from_value: *from_value,
            to_value: *to_value,
        },
        D::Unary(op, i) => D::Unary(*op, i),
        D::Binary(op, l, r) => D::Binary(*op, l, r),
        D::Spline(s) => D::Spline(s),
        D::Lerp {
            alpha,
            first,
            second,
        } => D::Lerp {
            alpha,
            first,
            second,
        },
        D::Clamp { input, min, max } => D::Clamp {
            input,
            min: *min,
            max: *max,
        },
        D::RangeChoice {
            input,
            min_inclusive,
            max_exclusive,
            when_in_range,
            when_out_of_range,
        } => D::RangeChoice {
            input,
            min_inclusive: *min_inclusive,
            max_exclusive: *max_exclusive,
            when_in_range,
            when_out_of_range,
        },
        D::IntervalSelect {
            input,
            thresholds,
            functions,
        } => D::IntervalSelect {
            input,
            thresholds,
            functions,
        },
        D::Cache(i) => D::Cache(i),
        D::BlendDensity(i) => D::BlendDensity(i),
        D::Interpolated {
            input,
            cell_xz,
            cell_y,
        } => D::Interpolated {
            input,
            cell_xz: *cell_xz,
            cell_y: *cell_y,
        },
        D::Slice {
            axis,
            coordinate,
            input,
        } => D::Slice {
            axis: *axis,
            coordinate: *coordinate,
            input,
        },
        D::FindTopSurface {
            density,
            upper_bound,
            lower_bound,
            cell_height,
        } => D::FindTopSurface {
            density,
            upper_bound,
            lower_bound: *lower_bound,
            cell_height: *cell_height,
        },
        D::OldBlendedNoise {
            xz_scale,
            y_scale,
            xz_factor,
            y_factor,
            smear_scale_multiplier,
        } => D::OldBlendedNoise {
            xz_scale: *xz_scale,
            y_scale: *y_scale,
            xz_factor: *xz_factor,
            y_factor: *y_factor,
            smear_scale_multiplier: *smear_scale_multiplier,
        },
        D::BlendAlpha => D::BlendAlpha,
        D::BlendOffset => D::BlendOffset,
        D::Beardifier => D::Beardifier,
    }
}

pub(super) fn clone_spline(s: &Spline) -> Spline {
    match s {
        Spline::Constant(v) => Spline::Constant(*v),
        Spline::Multipoint { coordinate, points } => Spline::Multipoint { coordinate, points },
    }
}

pub(super) fn clone_rule(r: &MaterialRule) -> MaterialRule {
    match r {
        MaterialRule::Reference(n) => MaterialRule::Reference(n),
        MaterialRule::Block(b) => MaterialRule::Block(*b),
        MaterialRule::Sequence(s) => MaterialRule::Sequence(s),
        MaterialRule::Condition { if_true, then_run } => {
            MaterialRule::Condition { if_true, then_run }
        }
        MaterialRule::Bandlands => MaterialRule::Bandlands,
        MaterialRule::OreVein {
            ore_block,
            raw_ore_block,
            filler_block,
            raw_ore_chance,
            density,
            richness,
            filler_gap,
        } => MaterialRule::OreVein {
            ore_block: *ore_block,
            raw_ore_block: *raw_ore_block,
            filler_block: *filler_block,
            raw_ore_chance: *raw_ore_chance,
            density,
            richness,
            filler_gap,
        },
    }
}

pub(super) fn clone_condition(c: &MaterialCondition) -> MaterialCondition {
    use MaterialCondition as C;
    match c {
        C::Reference(n) => C::Reference(n),
        C::Biome(b) => C::Biome(b),
        C::NoiseThreshold {
            noise,
            min_threshold,
            max_threshold,
            is_3d,
        } => C::NoiseThreshold {
            noise,
            min_threshold: *min_threshold,
            max_threshold: *max_threshold,
            is_3d: *is_3d,
        },
        C::VerticalGradient {
            random_name,
            true_at_and_below,
            false_at_and_above,
        } => C::VerticalGradient {
            random_name,
            true_at_and_below: *true_at_and_below,
            false_at_and_above: *false_at_and_above,
        },
        C::YAbove {
            anchor,
            surface_depth_multiplier,
            add_stone_depth,
        } => C::YAbove {
            anchor: *anchor,
            surface_depth_multiplier: *surface_depth_multiplier,
            add_stone_depth: *add_stone_depth,
        },
        C::Water {
            offset,
            surface_depth_multiplier,
            add_stone_depth,
        } => C::Water {
            offset: *offset,
            surface_depth_multiplier: *surface_depth_multiplier,
            add_stone_depth: *add_stone_depth,
        },
        C::StoneDepth {
            offset,
            add_surface_depth,
            secondary_depth_range,
            surface_type,
        } => C::StoneDepth {
            offset: *offset,
            add_surface_depth: *add_surface_depth,
            secondary_depth_range: *secondary_depth_range,
            surface_type: *surface_type,
        },
        C::Not(i) => C::Not(i),
        C::Steep => C::Steep,
        C::Hole => C::Hole,
        C::AbovePreliminarySurface => C::AbovePreliminarySurface,
        C::Temperature => C::Temperature,
    }
}
