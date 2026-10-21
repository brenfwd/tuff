use super::*;

pub fn int_provider(value: &Value) -> Result<String> {
    if let Some(v) = value.as_i64() {
        return Ok(format!("IntProvider::Constant({v})"));
    }
    let m = value
        .as_object()
        .context("int providers are numbers or objects")?;
    let kind = string(m, "type")?;
    let range = |fields: &[&str]| -> Result<(i64, i64)> {
        let m = object(value, fields)?;
        Ok((int(m, "min_inclusive")?, int(m, "max_inclusive")?))
    };
    Ok(match kind {
        "minecraft:constant" => {
            let m = object(value, &["type", "value"])?;
            format!("IntProvider::Constant({})", int(m, "value")?)
        }
        "minecraft:uniform" => {
            let (min, max) = range(&["type", "min_inclusive", "max_inclusive"])?;
            format!("IntProvider::Uniform {{ min: {min}, max: {max} }}")
        }
        "minecraft:biased_to_bottom" => {
            let (min, max) = range(&["type", "min_inclusive", "max_inclusive"])?;
            format!("IntProvider::BiasedToBottom {{ min: {min}, max: {max} }}")
        }
        "minecraft:very_biased_to_bottom" => {
            let (min, max) = range(&["type", "min_inclusive", "max_inclusive"])?;
            format!("IntProvider::VeryBiasedToBottom {{ min: {min}, max: {max} }}")
        }
        "minecraft:clamped" => {
            let m = object(value, &["type", "source", "min_inclusive", "max_inclusive"])?;
            format!(
                "IntProvider::Clamped {{ source: &{}, min: {}, max: {} }}",
                int_provider(field(m, "source")?)?,
                int(m, "min_inclusive")?,
                int(m, "max_inclusive")?
            )
        }
        "minecraft:clamped_normal" => {
            let m = object(
                value,
                &[
                    "type",
                    "mean",
                    "deviation",
                    "min_inclusive",
                    "max_inclusive",
                ],
            )?;
            format!(
                "IntProvider::ClampedNormal {{ mean: {}, deviation: {}, min: {}, max: {} }}",
                f32_lit(num(m, "mean")? as f32),
                f32_lit(num(m, "deviation")? as f32),
                int(m, "min_inclusive")?,
                int(m, "max_inclusive")?
            )
        }
        "minecraft:trapezoid" => {
            let m = object(value, &["type", "min", "max", "plateau"])?;
            format!(
                "IntProvider::Trapezoid {{ min: {}, max: {}, plateau: {} }}",
                int(m, "min")?,
                int(m, "max")?,
                int(m, "plateau")?
            )
        }
        "minecraft:weighted_list" => {
            let m = object(value, &["type", "distribution"])?;
            let mut entries = Vec::new();
            for entry in field(m, "distribution")?
                .as_array()
                .context("distribution")?
            {
                let e = object(entry, &["data", "weight"])?;
                entries.push(format!(
                    "(&{}, {})",
                    int_provider(field(e, "data")?)?,
                    int(e, "weight")?
                ));
            }
            ensure!(!entries.is_empty(), "weighted lists are non-empty");
            format!("IntProvider::WeightedList(&[{}])", entries.join(", "))
        }
        other => bail!("unmodeled int provider {other}"),
    })
}

pub fn float_provider(value: &Value) -> Result<String> {
    if let Some(v) = value.as_f64() {
        return Ok(format!("FloatProvider::Constant({})", f32_lit(v as f32)));
    }
    let m = value
        .as_object()
        .context("float providers are numbers or objects")?;
    Ok(match string(m, "type")? {
        "minecraft:constant" => {
            let m = object(value, &["type", "value"])?;
            format!(
                "FloatProvider::Constant({})",
                f32_lit(num(m, "value")? as f32)
            )
        }
        "minecraft:uniform" => {
            let m = object(value, &["type", "min_inclusive", "max_exclusive"])?;
            format!(
                "FloatProvider::Uniform {{ min: {}, max: {} }}",
                f32_lit(num(m, "min_inclusive")? as f32),
                f32_lit(num(m, "max_exclusive")? as f32)
            )
        }
        "minecraft:clamped_normal" => {
            let m = object(value, &["type", "mean", "deviation", "min", "max"])?;
            format!(
                "FloatProvider::ClampedNormal {{ mean: {}, deviation: {}, min: {}, max: {} }}",
                f32_lit(num(m, "mean")? as f32),
                f32_lit(num(m, "deviation")? as f32),
                f32_lit(num(m, "min")? as f32),
                f32_lit(num(m, "max")? as f32)
            )
        }
        "minecraft:trapezoid" => {
            let m = object(value, &["type", "min", "max", "plateau"])?;
            format!(
                "FloatProvider::Trapezoid {{ min: {}, max: {}, plateau: {} }}",
                f32_lit(num(m, "min")? as f32),
                f32_lit(num(m, "max")? as f32),
                f32_lit(num(m, "plateau")? as f32)
            )
        }
        other => bail!("unmodeled float provider {other}"),
    })
}

pub fn anchor_lit(value: &Value) -> Result<String> {
    let m = value.as_object().context("anchors are objects")?;
    ensure!(m.len() == 1, "anchors have one field");
    let (key, v) = m.iter().next().unwrap();
    let v = v.as_i64().context("anchor offsets are integers")?;
    Ok(match key.as_str() {
        "absolute" => format!("VerticalAnchor::Absolute({v})"),
        "above_bottom" => format!("VerticalAnchor::AboveBottom({v})"),
        "below_top" => format!("VerticalAnchor::BelowTop({v})"),
        "relative_to_sea_level" => format!("VerticalAnchor::RelativeToSeaLevel({v})"),
        other => bail!("unknown anchor {other}"),
    })
}

pub fn height_provider(value: &Value) -> Result<String> {
    let m = value.as_object().context("height providers are objects")?;
    if !m.contains_key("type") {
        return Ok(format!("HeightProvider::Constant({})", anchor_lit(value)?));
    }
    Ok(match string(m, "type")? {
        "minecraft:constant" => {
            let m = object(value, &["type", "value"])?;
            format!(
                "HeightProvider::Constant({})",
                anchor_lit(field(m, "value")?)?
            )
        }
        "minecraft:uniform" => {
            let m = object(value, &["type", "min_inclusive", "max_inclusive"])?;
            format!(
                "HeightProvider::Uniform {{ min: {}, max: {} }}",
                anchor_lit(field(m, "min_inclusive")?)?,
                anchor_lit(field(m, "max_inclusive")?)?
            )
        }
        kind @ ("minecraft:biased_to_bottom" | "minecraft:very_biased_to_bottom") => {
            let m = object(value, &["type", "min_inclusive", "max_inclusive", "inner"])?;
            let variant = if kind == "minecraft:biased_to_bottom" {
                "BiasedToBottom"
            } else {
                "VeryBiasedToBottom"
            };
            let inner = m.get("inner").and_then(Value::as_i64).unwrap_or(1);
            ensure!(inner >= 1, "inner must be positive");
            format!(
                "HeightProvider::{variant} {{ min: {}, max: {}, inner: {inner} }}",
                anchor_lit(field(m, "min_inclusive")?)?,
                anchor_lit(field(m, "max_inclusive")?)?
            )
        }
        "minecraft:trapezoid" => {
            let m = object(
                value,
                &["type", "min_inclusive", "max_inclusive", "plateau"],
            )?;
            format!(
                "HeightProvider::Trapezoid {{ min: {}, max: {}, plateau: {} }}",
                anchor_lit(field(m, "min_inclusive")?)?,
                anchor_lit(field(m, "max_inclusive")?)?,
                m.get("plateau").and_then(Value::as_i64).unwrap_or(0)
            )
        }
        "minecraft:weighted_list" => {
            let m = object(value, &["type", "distribution"])?;
            let mut entries = Vec::new();
            for entry in field(m, "distribution")?
                .as_array()
                .context("distribution")?
            {
                let e = object(entry, &["data", "weight"])?;
                entries.push(format!(
                    "(&{}, {})",
                    height_provider(field(e, "data")?)?,
                    int(e, "weight")?
                ));
            }
            format!("HeightProvider::WeightedList(&[{}])", entries.join(", "))
        }
        other => bail!("unmodeled height provider {other}"),
    })
}

pub(super) fn carver(value: &Value) -> Result<String> {
    let m = value.as_object().context("carvers are objects")?;
    let kind = string(m, "type")?;
    let config = match kind {
        "minecraft:cave" => {
            let m = object(
                value,
                &[
                    "type",
                    "probability",
                    "y",
                    "count",
                    "thickness",
                    "weird_thickness_bias",
                    "room_vertical_radius_multiplier",
                    "horizontal_radius_multiplier",
                    "vertical_radius_multiplier",
                    "start_vertical_radius_multiplier",
                    "floor_level",
                ],
            )?;
            let start = match m.get("start_vertical_radius_multiplier") {
                Some(v) => float_provider(v)?,
                None => "FloatProvider::Constant(1.0f32)".into(),
            };
            format!(
                "CarverConfig::Cave {{ probability: {}, y: &{}, count: &{}, thickness: &{}, weird_thickness_bias: {}, room_vertical_radius_multiplier: &{}, horizontal_radius_multiplier: &{}, vertical_radius_multiplier: &{}, start_vertical_radius_multiplier: &{start}, floor_level: &{} }}",
                f32_lit(num(m, "probability")? as f32),
                height_provider(field(m, "y")?)?,
                int_provider(field(m, "count")?)?,
                float_provider(field(m, "thickness")?)?,
                m.get("weird_thickness_bias")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                float_provider(field(m, "room_vertical_radius_multiplier")?)?,
                float_provider(field(m, "horizontal_radius_multiplier")?)?,
                float_provider(field(m, "vertical_radius_multiplier")?)?,
                float_provider(field(m, "floor_level")?)?
            )
        }
        "minecraft:canyon" => {
            let m = object(
                value,
                &["type", "probability", "y", "vertical_rotation", "shape"],
            )?;
            let shape = object(
                field(m, "shape")?,
                &[
                    "distance_factor",
                    "thickness",
                    "width_smoothness",
                    "horizontal_radius_factor",
                    "vertical_radius_default_factor",
                    "vertical_radius_center_factor",
                    "y_scale",
                ],
            )?;
            format!(
                "CarverConfig::Canyon {{ probability: {}, y: &{}, vertical_rotation: &{}, distance_factor: &{}, thickness: &{}, width_smoothness: {}, horizontal_radius_factor: &{}, vertical_radius_default_factor: {}, vertical_radius_center_factor: {}, y_scale: &{} }}",
                f32_lit(num(m, "probability")? as f32),
                height_provider(field(m, "y")?)?,
                float_provider(field(m, "vertical_rotation")?)?,
                float_provider(field(shape, "distance_factor")?)?,
                float_provider(field(shape, "thickness")?)?,
                int(shape, "width_smoothness")?,
                float_provider(field(shape, "horizontal_radius_factor")?)?,
                f32_lit(num(shape, "vertical_radius_default_factor")? as f32),
                f32_lit(num(shape, "vertical_radius_center_factor")? as f32),
                float_provider(field(shape, "y_scale")?)?
            )
        }
        other => bail!("unmodeled carver type {other}"),
    };
    Ok(format!("kind: {}, config: {config}", str_literal(kind)))
}

/// Top-level configured feature types and their fields. Nested sub-features
/// are checked when the runtime parses them.
pub(super) const FEATURE_FIELDS: &[(&str, &[&str])] = &[
    ("bamboo", &["probability"]),
    ("block_blob", &["can_place_on", "state"]),
    (
        "block_column",
        &["allowed_placement", "direction", "layers", "prioritize_tip"],
    ),
    ("block_pile", &["state_provider"]),
    ("blue_ice", &[]),
    ("bonus_chest", &[]),
    ("chorus_plant", &[]),
    ("delta_feature", &["contents", "rim", "rim_size", "size"]),
    (
        "disk",
        &["half_height", "radius", "state_provider", "target"],
    ),
    ("end_gateway", &["exact", "exit"]),
    ("end_island", &[]),
    ("end_platform", &[]),
    ("end_podium", &["active"]),
    (
        "end_spike",
        &["crystal_beam_target", "crystal_invulnerable", "spikes"],
    ),
    (
        "fallen_tree",
        &[
            "log_decorators",
            "log_length",
            "stump_decorators",
            "trunk_provider",
        ],
    ),
    (
        "fossil",
        &[
            "fossil_processors",
            "fossil_structures",
            "max_empty_corners_allowed",
            "overlay_processors",
            "overlay_structures",
        ],
    ),
    ("freeze_top_layer", &[]),
    (
        "geode",
        &[
            "blocks",
            "crack",
            "invalid_blocks_threshold",
            "layers",
            "outer_wall_distance",
            "use_alternate_layer0_chance",
            "use_potential_placements_chance",
            "distribution_points",
            "point_offset",
            "min_gen_offset",
            "max_gen_offset",
            "noise_multiplier",
        ],
    ),
    (
        "huge_brown_mushroom",
        &[
            "can_place_on",
            "cap_provider",
            "foliage_radius",
            "stem_provider",
        ],
    ),
    (
        "huge_fungus",
        &[
            "decor_state",
            "hat_state",
            "planted",
            "replaceable_blocks",
            "stem_state",
            "valid_base_block",
        ],
    ),
    (
        "huge_red_mushroom",
        &[
            "can_place_on",
            "cap_provider",
            "foliage_radius",
            "stem_provider",
        ],
    ),
    ("iceberg", &["state"]),
    (
        "lake",
        &[
            "barrier",
            "can_place_feature",
            "can_replace_with_air_or_fluid",
            "can_replace_with_barrier",
            "fluid",
        ],
    ),
    (
        "large_dripstone",
        &[
            "column_radius",
            "floor_to_ceiling_search_range",
            "height_scale",
            "max_column_radius_to_cave_height_ratio",
            "min_bluntness_for_wind",
            "min_radius_for_wind",
            "replaceable_blocks",
            "stalactite_bluntness",
            "stalagmite_bluntness",
            "wind_speed",
        ],
    ),
    ("monster_room", &[]),
    (
        "multiface_growth",
        &[
            "block",
            "can_be_placed_on",
            "can_place_on_ceiling",
            "can_place_on_floor",
            "can_place_on_wall",
            "chance_of_spreading",
            "search_range",
        ],
    ),
    ("netherrack_replace_blobs", &["radius", "state", "target"]),
    (
        "ore",
        &["discard_chance_on_air_exposure", "size", "targets"],
    ),
    ("overlay", &["features"]),
    (
        "random_boolean_selector",
        &["feature_false", "feature_true"],
    ),
    (
        "random_neighbor_spread",
        &[
            "accepted_neighbors",
            "attempts",
            "block",
            "can_replace",
            "xz_offset",
            "y_offset",
        ],
    ),
    ("random_selector", &["default", "features"]),
    (
        "root_system",
        &[
            "allowed_tree_position",
            "allowed_vertical_water_for_tree",
            "feature",
            "hanging_root_placement_attempts",
            "hanging_root_radius",
            "hanging_root_state_provider",
            "hanging_roots_vertical_span",
            "level_test_distance",
            "max_level_deviation",
            "required_vertical_space_for_tree",
            "root_column_max_height",
            "root_placement_attempts",
            "root_radius",
            "root_replaceable",
            "root_state_provider",
        ],
    ),
    (
        "scattered_ore",
        &["discard_chance_on_air_exposure", "size", "targets"],
    ),
    ("sequence", &["features"]),
    ("simple_block", &["schedule_tick", "to_place"]),
    ("simple_random_selector", &["features"]),
    (
        "speleothem_cluster",
        &[
            "base_block",
            "chance_of_speleothem_at_max_distance_from_center",
            "density",
            "floor_to_ceiling_search_range",
            "height",
            "height_deviation",
            "max_distance_from_center_affecting_height_bias",
            "max_distance_from_edge_affecting_chance_of_speleothem",
            "max_stalagmite_stalactite_height_diff",
            "pointed_block",
            "radius",
            "replaceable_blocks",
            "speleothem_block_layer_thickness",
            "wetness",
        ],
    ),
    ("spike", &["can_place_on", "can_replace", "state"]),
    (
        "spring_feature",
        &[
            "hole_count",
            "requires_block_below",
            "rock_count",
            "state",
            "valid_blocks",
        ],
    ),
    (
        "tree",
        &[
            "below_trunk_provider",
            "decorators",
            "foliage_placer",
            "foliage_provider",
            "ignore_vines",
            "minimum_size",
            "root_placer",
            "trunk_placer",
            "trunk_provider",
        ],
    ),
    (
        "underwater_magma",
        &[
            "floor_search_range",
            "placement_probability_per_valid_position",
            "placement_radius_around_floor",
        ],
    ),
    (
        "vegetation_patch",
        &[
            "depth",
            "extra_bottom_block_chance",
            "extra_edge_column_chance",
            "ground_state",
            "replaceable",
            "surface",
            "vegetation_chance",
            "vegetation_feature",
            "vertical_range",
            "xz_radius",
        ],
    ),
    ("vines", &[]),
    ("void_start_platform", &[]),
    (
        "waterlogged_vegetation_patch",
        &[
            "depth",
            "extra_bottom_block_chance",
            "extra_edge_column_chance",
            "ground_state",
            "replaceable",
            "surface",
            "vegetation_chance",
            "vegetation_feature",
            "vertical_range",
            "xz_radius",
        ],
    ),
    ("weighted_random_selector", &["features"]),
];

pub(super) const PLACEMENT_FIELDS: &[(&str, &[&str])] = &[
    ("biome", &[]),
    ("block_predicate_filter", &["predicate"]),
    ("count", &["count"]),
    ("count_on_every_layer", &["count"]),
    (
        "environment_scan",
        &[
            "allowed_search_condition",
            "direction_of_search",
            "max_steps",
            "target_condition",
        ],
    ),
    ("fixed_placement", &["positions"]),
    ("height_range", &["height"]),
    ("heightmap", &["heightmap"]),
    ("in_square", &[]),
    (
        "noise_based_count",
        &["noise_factor", "noise_offset", "noise_to_count_ratio"],
    ),
    (
        "noise_threshold_count",
        &["above_noise", "below_noise", "noise_level"],
    ),
    ("offset", &["x", "y", "z"]),
    ("random_chance", &["chance"]),
    ("randomly_selected", &["placements"]),
    ("rarity_filter", &["chance"]),
    (
        "surface_relative_threshold_filter",
        &["heightmap", "max_inclusive", "min_inclusive"],
    ),
    ("surface_water_depth_filter", &["max_water_depth"]),
];

pub(super) fn check_typed(value: &Value, table: &[(&str, &[&str])], what: &str) -> Result<()> {
    let m = value
        .as_object()
        .with_context(|| format!("{what} is an object"))?;
    let kind = qualify(string(m, "type")?);
    let short = kind
        .strip_prefix("minecraft:")
        .context("vanilla kinds only")?;
    let (_, fields) = table
        .iter()
        .find(|(k, _)| *k == short)
        .with_context(|| format!("unmodeled {what} type {kind}"))?;
    for key in m.keys() {
        ensure!(
            key == "type" || fields.contains(&key.as_str()),
            "unknown field {key} on {what} {kind}"
        );
    }
    Ok(())
}

pub(super) fn check_placed(value: &Value, configured: &BTreeMap<String, &Value>) -> Result<()> {
    let m = object(value, &["feature", "placement"])?;
    match field(m, "feature")? {
        Value::String(name) => ensure!(
            configured.contains_key(&qualify(name)),
            "unknown configured feature {name}"
        ),
        inline => check_typed(inline, FEATURE_FIELDS, "feature")?,
    }
    for modifier in field(m, "placement")?
        .as_array()
        .context("placement is a list")?
    {
        check_typed(modifier, PLACEMENT_FIELDS, "placement modifier")?;
        if let Some(Value::Array(list)) = modifier.get("placements") {
            for inner in list {
                check_typed(inner, PLACEMENT_FIELDS, "placement modifier")?;
            }
        }
    }
    Ok(())
}
