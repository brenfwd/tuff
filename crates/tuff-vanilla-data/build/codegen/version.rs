use std::fmt::Write;

use anyhow::{Context, Result};

use crate::input::Input;
use crate::names::str_literal;

pub fn generate(input: &Input) -> Result<String> {
    let v = &input.version;
    let p = &v.pack_version;
    let mut out = String::new();
    writeln!(out, "pub const ID: &str = {};", str_literal(&v.id))?;
    writeln!(
        out,
        "pub const BUILD_TIME: &str = {};",
        str_literal(&v.build_time)
    )?;
    writeln!(out, "pub const NAME: &str = {};", str_literal(&v.name))?;
    writeln!(
        out,
        "pub const PROTOCOL_VERSION: i32 = {};",
        v.protocol_version
    )?;
    writeln!(out, "pub const DATA_VERSION: i32 = {};", v.world_version)?;
    writeln!(
        out,
        "pub const SERIES: &str = {};",
        str_literal(&v.series_id)
    )?;
    writeln!(out, "pub const STABLE: bool = {};", v.stable)?;
    writeln!(
        out,
        "pub const DATA_PACK_VERSION: PackVersion = PackVersion {{ major: {}, minor: {} }};",
        p.data_major, p.data_minor
    )?;
    writeln!(
        out,
        "pub const RESOURCE_PACK_VERSION: PackVersion = PackVersion {{ major: {}, minor: {} }};",
        p.resource_major, p.resource_minor
    )?;
    writeln!(out, "pub const FEATURE_PACKS: &[FeaturePack] = &[")?;
    for (path, meta) in &input.data_json {
        let Some(name) = path
            .strip_prefix("minecraft/datapacks/")
            .and_then(|rest| rest.strip_suffix("/pack.mcmeta"))
        else {
            continue;
        };
        let features: Vec<String> = meta
            .pointer("/features/enabled")
            .and_then(|v| v.as_array())
            .context("feature pack without features.enabled")?
            .iter()
            .map(|flag| flag.as_str().map(str_literal).context("feature flag"))
            .collect::<Result<_>>()?;
        let description = meta
            .pointer("/pack/description/translate")
            .and_then(|v| v.as_str())
            .context("feature pack without a translated description")?;
        let root = format!("{}{name}/", crate::input::FEATURE_PACKS);
        let files = input
            .feature_pack_json
            .iter()
            .filter_map(|(path, json)| Some((path.strip_prefix(&root)?, json)))
            .map(|(path, json)| {
                Ok(format!(
                    "({}, {})",
                    str_literal(path),
                    str_literal(&serde_json::to_string(json)?)
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        writeln!(
            out,
            "    FeaturePack {{ id: {}, features: &[{}], description: {}, files: &[{}] }},",
            str_literal(name),
            features.join(", "),
            str_literal(description),
            files.join(", ")
        )?;
    }
    writeln!(out, "];")?;
    Ok(out)
}
