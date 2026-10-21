// SPDX-License-Identifier: MIT
//! Typed builders for the dimension type and biome registry JSON a plugin
//! hands to [`Plugin::dimensions`](crate::Plugin::dimensions). Raw JSON stays
//! available through [`DimensionType::raw`] and [`BiomeDef::raw`].

use crate::Biome;

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn num(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// Fixed-time sky, light and gameplay settings of a dimension type.
#[derive(Clone, Debug)]
pub struct DimensionType {
    raw: Option<String>,
    ambient_light: f64,
    attributes: Vec<(String, String)>,
    coordinate_scale: f64,
    has_ceiling: bool,
    has_ender_dragon_fight: bool,
    has_fixed_time: bool,
    has_skylight: bool,
    height: i32,
    infiniburn: String,
    logical_height: i32,
    min_y: i32,
    monster_spawn_block_light_limit: i32,
    monster_spawn_light_level: i32,
    skybox: String,
}

impl Default for DimensionType {
    fn default() -> Self {
        Self {
            raw: None,
            ambient_light: 0.0,
            attributes: Vec::new(),
            coordinate_scale: 1.0,
            has_ceiling: false,
            has_ender_dragon_fight: false,
            has_fixed_time: false,
            has_skylight: true,
            height: 384,
            infiniburn: "#minecraft:infiniburn_overworld".into(),
            logical_height: 384,
            min_y: -64,
            monster_spawn_block_light_limit: 0,
            monster_spawn_light_level: 7,
            skybox: "overworld".into(),
        }
    }
}

impl DimensionType {
    pub fn new() -> Self {
        Self::default()
    }

    /// Escape hatch: uses this JSON verbatim, ignoring every other setter.
    pub fn raw(json: impl Into<String>) -> Self {
        Self {
            raw: Some(json.into()),
            ..Self::default()
        }
    }

    pub fn ambient_light(mut self, v: f64) -> Self {
        self.ambient_light = v;
        self
    }

    pub fn coordinate_scale(mut self, v: f64) -> Self {
        self.coordinate_scale = v;
        self
    }

    pub fn has_ceiling(mut self, v: bool) -> Self {
        self.has_ceiling = v;
        self
    }

    pub fn has_ender_dragon_fight(mut self, v: bool) -> Self {
        self.has_ender_dragon_fight = v;
        self
    }

    pub fn has_fixed_time(mut self, v: bool) -> Self {
        self.has_fixed_time = v;
        self
    }

    pub fn has_skylight(mut self, v: bool) -> Self {
        self.has_skylight = v;
        self
    }

    /// Sets `min_y` and `height`, and `logical_height` to the same height.
    pub fn range(mut self, min_y: i32, height: i32) -> Self {
        self.min_y = min_y;
        self.height = height;
        self.logical_height = height;
        self
    }

    pub fn logical_height(mut self, v: i32) -> Self {
        self.logical_height = v;
        self
    }

    /// A block tag key such as `#minecraft:infiniburn_overworld`.
    pub fn infiniburn(mut self, tag: impl Into<String>) -> Self {
        self.infiniburn = tag.into();
        self
    }

    pub fn monster_spawn(mut self, block_light_limit: i32, light_level: i32) -> Self {
        self.monster_spawn_block_light_limit = block_light_limit;
        self.monster_spawn_light_level = light_level;
        self
    }

    pub fn skybox(mut self, v: impl Into<String>) -> Self {
        self.skybox = v.into();
        self
    }

    /// A boolean environment attribute, e.g. `minecraft:gameplay/water_evaporates`.
    pub fn attribute_bool(mut self, key: &str, v: bool) -> Self {
        self.attributes.push((key.into(), v.to_string()));
        self
    }

    /// A color attribute as `#rrggbb`, e.g. `minecraft:visual/sky_color`.
    pub fn attribute_color(mut self, key: &str, hex: &str) -> Self {
        self.attributes.push((key.into(), quote(hex)));
        self
    }

    /// An attribute with a hand-written JSON value.
    pub fn attribute_json(mut self, key: &str, json: &str) -> Self {
        self.attributes.push((key.into(), json.into()));
        self
    }

    /// Beds cannot be slept in or set spawn, with the stock error message.
    pub fn bed_unusable(self) -> Self {
        self.attribute_json(
            "minecraft:gameplay/bed_rule",
            r#"{"can_set_spawn":"never","can_sleep":"never","error_message":{"translate":"block.minecraft.bed.no_sleep"}}"#,
        )
    }

    /// Ambient particles, each `(particle type, probability)`.
    pub fn ambient_particles(self, particles: &[(&str, f64)]) -> Self {
        let items: Vec<String> = particles
            .iter()
            .map(|(ty, p)| {
                format!(
                    "{{\"particle\":{{\"type\":{}}},\"probability\":{}}}",
                    quote(ty),
                    num(*p)
                )
            })
            .collect();
        self.attribute_json(
            "minecraft:visual/ambient_particles",
            &format!("[{}]", items.join(",")),
        )
    }

    pub fn to_json(&self) -> String {
        if let Some(raw) = &self.raw {
            return raw.clone();
        }
        let attrs: Vec<String> = self
            .attributes
            .iter()
            .map(|(k, v)| format!("{}:{v}", quote(k)))
            .collect();
        format!(
            "{{\"ambient_light\":{},\"attributes\":{{{}}},\"coordinate_scale\":{},\"has_ceiling\":{},\"has_ender_dragon_fight\":{},\"has_fixed_time\":{},\"has_skylight\":{},\"height\":{},\"infiniburn\":{},\"logical_height\":{},\"min_y\":{},\"monster_spawn_block_light_limit\":{},\"monster_spawn_light_level\":{},\"skybox\":{}}}",
            num(self.ambient_light),
            attrs.join(","),
            num(self.coordinate_scale),
            self.has_ceiling,
            self.has_ender_dragon_fight,
            self.has_fixed_time,
            self.has_skylight,
            self.height,
            quote(&self.infiniburn),
            self.logical_height,
            self.min_y,
            self.monster_spawn_block_light_limit,
            self.monster_spawn_light_level,
            quote(&self.skybox),
        )
    }
}

/// A biome with no carvers or features.
#[derive(Clone, Debug)]
pub struct BiomeDef {
    raw: Option<String>,
    has_precipitation: bool,
    temperature: f64,
    downfall: f64,
    water_color: Option<String>,
}

impl Default for BiomeDef {
    fn default() -> Self {
        Self {
            raw: None,
            has_precipitation: true,
            temperature: 0.8,
            downfall: 0.4,
            water_color: None,
        }
    }
}

impl BiomeDef {
    pub fn new() -> Self {
        Self::default()
    }

    /// Escape hatch: uses this JSON verbatim, ignoring every other setter.
    pub fn raw(json: impl Into<String>) -> Self {
        Self {
            raw: Some(json.into()),
            ..Self::default()
        }
    }

    pub fn has_precipitation(mut self, v: bool) -> Self {
        self.has_precipitation = v;
        self
    }

    pub fn temperature(mut self, v: f64) -> Self {
        self.temperature = v;
        self
    }

    pub fn downfall(mut self, v: f64) -> Self {
        self.downfall = v;
        self
    }

    /// Water color as `#rrggbb`.
    pub fn water_color(mut self, hex: impl Into<String>) -> Self {
        self.water_color = Some(hex.into());
        self
    }

    pub fn to_json(&self) -> String {
        if let Some(raw) = &self.raw {
            return raw.clone();
        }
        let water = self
            .water_color
            .as_deref()
            .map(|c| format!("\"water_color\":{}", quote(c)))
            .unwrap_or_default();
        format!(
            "{{\"has_precipitation\":{},\"temperature\":{},\"downfall\":{},\"effects\":{{{water}}},\"carvers\":[],\"features\":[]}}",
            self.has_precipitation,
            num(self.temperature),
            num(self.downfall),
        )
    }

    /// The registry entry under `key`.
    pub fn into_biome(self, key: &str) -> Biome {
        Biome {
            key: key.into(),
            json: self.to_json(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn dimension_type_json_is_valid_shape() {
        let json = DimensionType::new()
            .range(0, 256)
            .bed_unusable()
            .attribute_color("minecraft:visual/sky_color", "#8fa4dc")
            .ambient_particles(&[("minecraft:white_ash", 0.004)])
            .to_json();
        assert!(json.starts_with("{\"ambient_light\":0.0,"));
        assert!(json.contains("\"min_y\":0,"));
        assert!(json.contains("\"height\":256,"));
        assert!(json.contains("\"minecraft:visual/sky_color\":\"#8fa4dc\""));
        assert!(json.contains("\"probability\":0.004"));
        assert_eq!(json.matches('{').count(), json.matches('}').count());
    }

    #[tuff_core::watchdog]
    #[test]
    fn biome_json_and_raw_escape_hatch() {
        let b = BiomeDef::new()
            .has_precipitation(false)
            .temperature(0.4)
            .downfall(0.0)
            .water_color("#4f7fd8")
            .into_biome("p:b");
        assert_eq!(b.key, "p:b");
        assert_eq!(
            b.json,
            "{\"has_precipitation\":false,\"temperature\":0.4,\"downfall\":0.0,\"effects\":{\"water_color\":\"#4f7fd8\"},\"carvers\":[],\"features\":[]}"
        );
        assert_eq!(BiomeDef::raw("{}").to_json(), "{}");
        assert_eq!(DimensionType::raw("{\"a\":1}").to_json(), "{\"a\":1}");
    }
}
