use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionMeta {
    pub id: String,
    pub main_class: String,
    pub asset_index: AssetIndexRef,
    pub downloads: Downloads,
    pub java_version: JavaVersion,
    pub libraries: Vec<Library>,
    pub arguments: Arguments,
}

#[derive(Debug, Deserialize)]
pub struct AssetIndexRef {
    pub id: String,
    pub sha1: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct Downloads {
    pub client: Download,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Download {
    pub sha1: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct JavaVersion {
    pub component: String,
}

#[derive(Debug, Deserialize)]
pub struct Library {
    pub name: String,
    pub downloads: LibraryDownloads,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
pub struct LibraryDownloads {
    pub artifact: Option<Artifact>,
}

#[derive(Debug, Deserialize)]
pub struct Artifact {
    pub path: String,
    pub sha1: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct Arguments {
    pub game: Vec<Arg>,
    pub jvm: Vec<Arg>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Arg {
    Plain(String),
    Conditional(ConditionalArg),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalArg {
    pub rules: Vec<Rule>,
    pub value: OneOrMany,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Allow,
    Disallow,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub action: Action,
    pub os: Option<OsRule>,
    pub features: Option<BTreeMap<String, bool>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OsRule {
    pub name: Option<String>,
    pub arch: Option<String>,
    pub version: Option<String>,
    #[serde(rename = "versionRange")]
    pub version_range: Option<VersionRange>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionRange {
    pub min: Option<String>,
    pub max: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub os: &'static str,
    pub arch: &'static str,
}

impl Platform {
    pub fn current() -> Self {
        use std::env::consts::{ARCH, OS};
        Self {
            os: match OS {
                "macos" => "osx",
                "windows" => "windows",
                _ => "linux",
            },
            arch: match ARCH {
                "x86" => "x86",
                "aarch64" => "arm64",
                _ => "x86_64",
            },
        }
    }
}

/// Features the harness turns on; every other feature is false.
pub const ENABLED_FEATURES: [&str; 3] = [
    "has_custom_resolution",
    "has_quick_plays_support",
    "is_quick_play_multiplayer",
];

fn feature_enabled(name: &str) -> bool {
    ENABLED_FEATURES.contains(&name)
}

impl Rule {
    fn matches(&self, platform: &Platform) -> bool {
        if let Some(os) = &self.os {
            if os.name.as_deref().is_some_and(|n| n != platform.os) {
                return false;
            }
            if os.arch.as_deref().is_some_and(|a| a != platform.arch) {
                return false;
            }
            // The host OS version is unknown: a minimum is taken as met, a
            // maximum as exceeded. The version regex is not evaluated.
            if os.version_range.as_ref().is_some_and(|r| r.max.is_some()) {
                return false;
            }
        }
        if let Some(features) = &self.features
            && features
                .iter()
                .any(|(name, wanted)| feature_enabled(name) != *wanted)
        {
            return false;
        }
        true
    }
}

/// No rules means allowed; otherwise the last matching rule decides.
pub fn allowed(rules: &[Rule], platform: &Platform) -> bool {
    if rules.is_empty() {
        return true;
    }
    rules
        .iter()
        .rfind(|rule| rule.matches(platform))
        .is_some_and(|rule| rule.action == Action::Allow)
}

impl VersionMeta {
    pub fn parse(json: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(json)?)
    }

    /// Libraries for this platform, in classpath order.
    pub fn libraries_for(&self, platform: &Platform) -> Result<Vec<&Artifact>> {
        let mut out = Vec::new();
        for library in &self.libraries {
            if !allowed(&library.rules, platform) {
                continue;
            }
            let artifact = library
                .downloads
                .artifact
                .as_ref()
                .ok_or_else(|| anyhow!("library {} has no artifact download", library.name))?;
            out.push(artifact);
        }
        Ok(out)
    }
}

/// Filters conditional arguments and substitutes `${name}` placeholders;
/// an unknown placeholder is an error.
pub fn expand_args(
    args: &[Arg],
    platform: &Platform,
    vars: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for arg in args {
        match arg {
            Arg::Plain(value) => out.push(substitute(value, vars)?),
            Arg::Conditional(c) if allowed(&c.rules, platform) => match &c.value {
                OneOrMany::One(v) => out.push(substitute(v, vars)?),
                OneOrMany::Many(vs) => {
                    for v in vs {
                        out.push(substitute(v, vars)?);
                    }
                }
            },
            Arg::Conditional(_) => {}
        }
    }
    Ok(out)
}

fn substitute(template: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| anyhow!("unterminated placeholder in {template:?}"))?;
        let name = &after[..end];
        match vars.get(name) {
            Some(value) => out.push_str(value),
            None => bail!("unknown placeholder ${{{name}}} in {template:?}"),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[derive(Debug, Deserialize)]
pub struct AssetIndex {
    pub objects: BTreeMap<String, AssetObject>,
}

#[derive(Debug, Deserialize)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}
