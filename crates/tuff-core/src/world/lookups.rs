use super::*;

pub fn registry_id(registry: &DataRegistry, name: &str) -> i32 {
    registry
        .entries
        .binary_search(&name)
        .unwrap_or_else(|_| panic!("{name} missing from {}", registry.key)) as i32
}

/// `kind` as the world's data packs leave it: a pack override's message,
/// scaling, exhaustion, effects and death message type, else `kind`.
pub fn effective_damage_type(kind: &DamageType) -> &DamageType {
    type Cache = Vec<(
        std::sync::Weak<tuff_item::registries::Registries>,
        std::collections::HashMap<&'static str, &'static DamageType>,
    )>;
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let registries = tuff_item::registries::current();
    let Some(frozen) = registries
        .get(data_registry::DAMAGE_TYPE.key)
        .filter(|r| r.modified())
    else {
        return kind;
    };
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.retain(|(owner, _)| owner.strong_count() > 0);
    let position = cache
        .iter()
        .position(|(owner, _)| std::ptr::eq(owner.as_ptr(), std::sync::Arc::as_ptr(&registries)));
    let index = match position {
        Some(index) => index,
        None => {
            let decoded = registries
                .decode::<tuff_item::registries::codecs::DamageTypes>()
                .ok();
            let mut overrides = std::collections::HashMap::new();
            for (id, entry) in frozen.iter() {
                if entry.is_builtin() {
                    continue;
                }
                let (Some(value), Some(builtin)) = (
                    decoded.as_ref().and_then(|d| d.get(id)),
                    tuff_vanilla_data::damage_type::ALL
                        .iter()
                        .find(|t| t.name == id),
                ) else {
                    continue;
                };
                use tuff_vanilla_data::damage_type::{DamageEffects, DeathMessageType, Scaling};
                let leaked: &'static DamageType = Box::leak(Box::new(DamageType {
                    name: builtin.name,
                    message_id: tuff_item::registries::intern(&value.message_id),
                    exhaustion: value.exhaustion,
                    scaling: Scaling::from_name(value.scaling).unwrap_or(builtin.scaling),
                    death_message_type: DeathMessageType::from_name(value.death_message_type),
                    effects: DamageEffects::from_name(value.effects),
                }));
                overrides.insert(builtin.name, leaked);
            }
            cache.push((std::sync::Arc::downgrade(&registries), overrides));
            cache.len() - 1
        }
    };
    match cache[index].1.get(kind.name) {
        Some(found) => found,
        None => kind,
    }
}

pub fn damage_type_id(kind: &DamageType) -> i32 {
    let registries = tuff_item::registries::current();
    match registries.get(data_registry::DAMAGE_TYPE.key) {
        Some(frozen) if frozen.modified() => frozen
            .index_of(&format!(
                "minecraft:{}",
                kind.name.trim_start_matches("minecraft:")
            ))
            .map_or(0, |id| id as i32),
        _ => registry_id(&data_registry::DAMAGE_TYPE, kind.name),
    }
}

/// Whether it is dark enough to sleep in `dimension`, from its sky light
/// timelines at the current clock times and the weather.
pub fn is_dark(shared: &Shared, dimension: &dimension::Dimension) -> bool {
    let clocks = shared.clocks.borrow();
    let weather = *shared.weather.borrow();
    dimension.environment().is_dark(
        |clock| clock::clock_id(clock).map_or(0, |id| clocks.get(id).total_ticks),
        weather.rain_level,
        weather.thunder_level * weather.rain_level,
    )
}

/// How many levels the sky light is darkened by in `dimension` now (time
/// of day and weather), as mobs and spawning see it.
pub fn sky_darken(shared: &Shared, dimension: &dimension::Dimension) -> u8 {
    let clocks = shared.clocks.borrow();
    let weather = *shared.weather.borrow();
    let light = dimension.environment().sky_light(
        |clock| clock::clock_id(clock).map_or(0, |id| clocks.get(id).total_ticks),
        weather.rain_level,
        weather.thunder_level * weather.rain_level,
    );
    (15.0 - light).clamp(0.0, 15.0) as u8
}

/// Whether undead mobs burn in the sun in `dimension` now.
pub fn monsters_burn(shared: &Shared, dimension: &dimension::Dimension) -> bool {
    let clocks = shared.clocks.borrow();
    dimension
        .environment()
        .monsters_burn(|clock| clock::clock_id(clock).map_or(0, |id| clocks.get(id).total_ticks))
}

/// Whether creaking hearts are awake in `dimension` now.
pub fn creaking_active(shared: &Shared, dimension: &dimension::Dimension) -> bool {
    let clocks = shared.clocks.borrow();
    dimension
        .environment()
        .creaking_active(|clock| clock::clock_id(clock).map_or(0, |id| clocks.get(id).total_ticks))
}

/// Whether bees stay in their hives in `dimension` now (night or rain).
pub fn bees_stay_in_hive(shared: &Shared, dimension: &dimension::Dimension) -> bool {
    let clocks = shared.clocks.borrow();
    let weather = *shared.weather.borrow();
    dimension.environment().bees_stay_in_hive(
        |clock| clock::clock_id(clock).map_or(0, |id| clocks.get(id).total_ticks),
        weather.rain_level,
        weather.thunder_level * weather.rain_level,
    )
}

/// The chance slimes spawn on swamp surfaces in `dimension` now.
pub fn surface_slime_chance(shared: &Shared, dimension: &dimension::Dimension) -> f32 {
    let clocks = shared.clocks.borrow();
    dimension.environment().surface_slime_spawn_chance(|clock| {
        clock::clock_id(clock).map_or(0, |id| clocks.get(id).total_ticks)
    })
}

/// How bright the moon is in `dimension` now, 0 at the new moon up to 1
/// at the full moon (twice the surface slime chance, which follows it).
pub fn moon_brightness(shared: &Shared, dimension: DimensionId) -> f32 {
    (surface_slime_chance(shared, shared.dimension(dimension)) * 2.0).clamp(0.0, 1.0)
}

/// Whether a bed rule condition holds in `dimension` now.
pub fn bed_condition(
    condition: tuff_vanilla_data::world::BedCondition,
    shared: &Shared,
    dimension: &dimension::Dimension,
) -> bool {
    match condition {
        tuff_vanilla_data::world::BedCondition::Always => true,
        tuff_vanilla_data::world::BedCondition::Never => false,
        tuff_vanilla_data::world::BedCondition::WhenDark => is_dark(shared, dimension),
    }
}

/// Whether a damage type is in the damage type tag `tag` (with or without
/// its `#`) in the tags in effect.
pub fn damage_type_in_tag(kind: &DamageType, tag: &str) -> bool {
    let tag = qualified_tag(tag);
    tuff_datapack::tags::with(|tags| tags.contains("minecraft:damage_type", &tag, kind.name))
}

/// Whether an entity type is in the entity type tag `tag` (with or without
/// its `#` and namespace) in the tags in effect.
pub fn entity_in_tag(kind: tuff_vanilla_data::registry::EntityType, tag: &str) -> bool {
    let tag = qualified_tag(tag);
    tuff_datapack::tags::with(|tags| tags.contains("minecraft:entity_type", &tag, kind.name()))
}

pub(super) fn qualified_tag(tag: &str) -> std::borrow::Cow<'_, str> {
    let tag = tag.strip_prefix('#').unwrap_or(tag);
    if tag.contains(':') {
        tag.into()
    } else {
        format!("minecraft:{tag}").into()
    }
}

pub fn framed<P: Encode>(packet: &P) -> Bytes {
    frame(packet, Some(COMPRESSION_THRESHOLD))
}

pub fn packets(packets: impl IntoIterator<Item = Bytes>) -> Packets {
    packets.into_iter().collect()
}

pub fn distance_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

pub fn message_plain(message: &TextComponent) -> String {
    message.plain()
}

#[cfg(test)]
mod effective_damage_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_pack_damage_type_override_changes_what_the_server_reads() {
        let mut loader = tuff_item::registries::Loader::default();
        loader.insert(
            &data_registry::DAMAGE_TYPE,
            "minecraft:arrow".to_owned(),
            "p",
            br#"{"message_id":"zapped","scaling":"always","exhaustion":0.5,"death_message_type":"intentional_game_design"}"#
                .to_vec(),
        );
        let frozen = loader.freeze().unwrap_or_else(|e| panic!("{e}"));
        let arrow = &tuff_vanilla_data::damage_type::ARROW;
        assert_eq!(effective_damage_type(arrow).message_id, arrow.message_id);
        tuff_item::registries::sync_scope(frozen, || {
            let effective = effective_damage_type(arrow);
            assert_eq!(effective.message_id, "zapped");
            assert_eq!(effective.exhaustion, 0.5);
            assert_eq!(
                effective.scaling,
                tuff_vanilla_data::damage_type::Scaling::Always
            );
            assert_eq!(
                effective.death_message_type,
                Some(tuff_vanilla_data::damage_type::DeathMessageType::IntentionalGameDesign)
            );
            let cactus = &tuff_vanilla_data::damage_type::CACTUS;
            assert!(std::ptr::eq(effective_damage_type(cactus), cactus));
        });
    }
}
