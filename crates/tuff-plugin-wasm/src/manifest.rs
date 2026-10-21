//! A plugin module's manifest: a `tuff-plugin` custom section of
//! `key = value` lines, read straight from the module's bytes before any
//! of its code is compiled or run.
//!
//! ```text
//! id = greeter
//! version = 1.2.0
//! api = 0
//! depends = economy >= 2.0.0, chat-format
//! description = Says hello
//! cap.http = discord.com/api/webhooks/ -- Relays chat to Discord
//! ```
//!
//! `cap.<kind>` lines request capabilities (see [`crate::caps`]); the
//! owner's grants decide what is allowed.

use std::cmp::Ordering;
use std::fmt;

use crate::caps;

/// The custom section that holds the manifest.
pub const SECTION: &str = "tuff-plugin";
/// The largest module the host reads.
pub const MAX_MODULE_BYTES: usize = 16 << 20;
/// The largest manifest section.
pub const MAX_MANIFEST_BYTES: usize = 16 << 10;
const MAX_ID_LEN: usize = 64;
const MAX_DEPENDENCIES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split('.');
        let mut next = || -> Option<u32> {
            let part = parts.next()?;
            if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let version = Self::new(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Another plugin that must be loaded first, at `min` or newer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub id: String,
    pub min: Option<Version>,
}

impl Dependency {
    pub fn accepts(&self, version: Version) -> bool {
        self.min.is_none_or(|min| version >= min)
    }
}

impl fmt::Display for Dependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.min {
            Some(min) => write!(f, "{} >= {min}", self.id),
            None => f.write_str(&self.id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub version: Version,
    pub api: u32,
    pub depends: Vec<Dependency>,
    /// `soft-depends`: loaded after these when present and accepted.
    pub soft_depends: Vec<Dependency>,
    /// `conflicts`: refused while a matching plugin is loaded, both ways.
    pub conflicts: Vec<Dependency>,
    /// `load-before`: ordering only, as if these soft-depended on this plugin.
    pub load_before: Vec<String>,
    pub description: String,
    pub capabilities: Vec<caps::Request>,
    /// `spawn_dimension`: the plugin dimension it wants players to spawn
    /// in, honoured only for a dimension it declares and was granted.
    pub spawn_dimension: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ManifestError {
    TooLarge(usize),
    NotWasm,
    Truncated,
    Missing,
    Duplicate,
    Malformed(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge(len) => write!(
                f,
                "module is {len} bytes, over the {MAX_MODULE_BYTES} byte limit"
            ),
            Self::NotWasm => f.write_str("not a WebAssembly module"),
            Self::Truncated => f.write_str("module is truncated"),
            Self::Missing => write!(f, "module has no {SECTION:?} custom section"),
            Self::Duplicate => write!(f, "module has more than one {SECTION:?} section"),
            Self::Malformed(why) => write!(f, "bad manifest: {why}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Whether `id` is a valid plugin id: 1 to 64 of `a-z 0-9 - _`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], ManifestError> {
        if len > self.bytes.len() {
            return Err(ManifestError::Truncated);
        }
        let (head, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(head)
    }

    fn u32(&mut self) -> Result<u32, ManifestError> {
        let mut value = 0u32;
        for i in 0..5 {
            let byte = *self.take(1)?.first().ok_or(ManifestError::Truncated)?;
            let bits = u32::from(byte & 0x7f);
            if i == 4 && bits > 0x0f {
                return Err(ManifestError::Malformed("oversized LEB128".into()));
            }
            value |= bits << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(ManifestError::Malformed("oversized LEB128".into()))
    }
}

/// The manifest of `module`, from its section headers alone.
pub fn read(module: &[u8]) -> Result<Manifest, ManifestError> {
    if module.len() > MAX_MODULE_BYTES {
        return Err(ManifestError::TooLarge(module.len()));
    }
    let mut reader = Reader { bytes: module };
    if reader.take(8).ok() != Some(b"\0asm\x01\0\0\0".as_slice()) {
        return Err(ManifestError::NotWasm);
    }
    let mut found = None;
    while !reader.bytes.is_empty() {
        let id = reader.take(1)?[0];
        let size = reader.u32()? as usize;
        let body = reader.take(size)?;
        if id != 0 {
            continue;
        }
        let mut section = Reader { bytes: body };
        let name_len = section.u32()? as usize;
        if section.take(name_len)? != SECTION.as_bytes() {
            continue;
        }
        if found.is_some() {
            return Err(ManifestError::Duplicate);
        }
        if section.bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::Malformed(format!(
                "section is over {MAX_MANIFEST_BYTES} bytes"
            )));
        }
        found = Some(section.bytes);
    }
    let text = std::str::from_utf8(found.ok_or(ManifestError::Missing)?)
        .map_err(|_| ManifestError::Malformed("not UTF-8".into()))?;
    parse(text)
}

fn malformed(why: impl Into<String>) -> ManifestError {
    ManifestError::Malformed(why.into())
}

/// Parses manifest text. Unknown keys are ignored.
pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
    let (mut id, mut version, mut api) = (None, None, None);
    let (mut depends, mut soft_depends, mut conflicts) = (Vec::new(), Vec::new(), Vec::new());
    let mut load_before = Vec::new();
    let mut description = String::new();
    let mut capabilities = Vec::new();
    let mut spawn_dimension = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| malformed(format!("line {line:?} is not key = value")))?;
        let (key, value) = (key.trim(), value.trim());
        if let Some(requests) = caps::Request::parse_line(key, value) {
            capabilities.extend(requests.map_err(malformed)?);
            if capabilities.len() > caps::policy::MAX_REQUESTS {
                return Err(malformed("too many capability requests"));
            }
            continue;
        }
        match key {
            "id" => {
                if !valid_id(value) {
                    return Err(malformed(format!(
                        "id {value:?} must be 1 to {MAX_ID_LEN} of a-z 0-9 - _"
                    )));
                }
                id = Some(value.to_owned());
            }
            "version" => {
                version = Some(
                    Version::parse(value)
                        .ok_or_else(|| malformed(format!("version {value:?} is not x.y.z")))?,
                );
            }
            "api" => {
                api = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| malformed(format!("api {value:?} is not a number")))?,
                );
            }
            "depends" => {
                for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                    depends.push(dependency(entry)?);
                }
            }
            "soft-depends" => {
                for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                    soft_depends.push(dependency(entry)?);
                }
            }
            "conflicts" => {
                for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                    conflicts.push(dependency(entry)?);
                }
            }
            "load-before" => {
                for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                    let dep = dependency(entry)?;
                    if dep.min.is_some() {
                        return Err(malformed("load-before takes plain ids"));
                    }
                    load_before.push(dep.id);
                }
            }
            "description" => description = value.to_owned(),
            "spawn_dimension" if !value.is_empty() => spawn_dimension = Some(value.to_owned()),
            _ => {}
        }
    }
    let id = id.ok_or_else(|| malformed("no id"))?;
    if depends.len() + soft_depends.len() + conflicts.len() + load_before.len() > MAX_DEPENDENCIES {
        return Err(malformed("too many dependencies"));
    }
    let names = |list: &[Dependency]| list.iter().map(|d| d.id.clone()).collect::<Vec<_>>();
    let related = [
        names(&depends),
        names(&soft_depends),
        names(&conflicts),
        load_before.clone(),
    ];
    if related.iter().flatten().any(|other| *other == id) {
        return Err(malformed("a plugin cannot name itself in a dependency key"));
    }
    for conflict in &conflicts {
        if depends
            .iter()
            .chain(&soft_depends)
            .any(|dep| dep.id == conflict.id)
        {
            return Err(malformed(format!(
                "{} is both depended on and in conflicts",
                conflict.id
            )));
        }
    }
    Ok(Manifest {
        version: version.ok_or_else(|| malformed("no version"))?,
        api: api.ok_or_else(|| malformed("no api"))?,
        id,
        depends,
        soft_depends,
        conflicts,
        load_before,
        description,
        capabilities,
        spawn_dimension,
    })
}

fn dependency(entry: &str) -> Result<Dependency, ManifestError> {
    let (id, min) = match entry.split_once(">=") {
        Some((id, min)) => (
            id.trim(),
            Some(
                Version::parse(min)
                    .ok_or_else(|| malformed(format!("dependency {entry:?} has a bad version")))?,
            ),
        ),
        None => (entry, None),
    };
    if !valid_id(id) {
        return Err(malformed(format!("dependency id {id:?} is not valid")));
    }
    Ok(Dependency {
        id: id.to_owned(),
        min,
    })
}

/// A custom section holding `payload` under `name`, for building test
/// modules.
pub fn custom_section(name: &str, payload: &[u8]) -> Vec<u8> {
    fn leb(mut value: usize, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }
    let mut body = Vec::new();
    leb(name.len(), &mut body);
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(payload);
    let mut section = vec![0];
    leb(body.len(), &mut section);
    section.extend(body);
    section
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &[u8] = b"\0asm\x01\0\0\0";

    fn module(sections: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = EMPTY.to_vec();
        for section in sections {
            bytes.extend_from_slice(section);
        }
        bytes
    }

    fn manifest(text: &str) -> Vec<u8> {
        custom_section(SECTION, text.as_bytes())
    }

    #[tuff_core::watchdog]
    #[test]
    fn reads_every_field() {
        let bytes = module(&[
            custom_section("name", b"\x00"),
            manifest(
                "# comment\nid = greeter\nversion = 1.2.3\napi = 0\n\
                 depends = economy >= 2.0.0, chat\ndescription = Says hi\nfuture = ignored\n\
                 spawn_dimension = greeter:hall\n",
            ),
        ]);
        let manifest = read(&bytes).unwrap();
        assert_eq!(manifest.id, "greeter");
        assert_eq!(manifest.version, Version::new(1, 2, 3));
        assert_eq!(manifest.api, 0);
        assert_eq!(manifest.description, "Says hi");
        assert_eq!(manifest.spawn_dimension.as_deref(), Some("greeter:hall"));
        assert_eq!(
            manifest.depends,
            [
                Dependency {
                    id: "economy".into(),
                    min: Some(Version::new(2, 0, 0))
                },
                Dependency {
                    id: "chat".into(),
                    min: None
                }
            ]
        );
        assert!(manifest.depends[0].accepts(Version::new(2, 1, 0)));
        assert!(!manifest.depends[0].accepts(Version::new(1, 9, 9)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn refuses_what_is_not_a_plugin() {
        assert_eq!(read(b"\x7fELF"), Err(ManifestError::NotWasm));
        assert_eq!(read(EMPTY), Err(ManifestError::Missing));
        let two = module(&[
            manifest("id=a\nversion=1.0.0\napi = 0"),
            manifest("id=a\nversion=1.0.0\napi = 0"),
        ]);
        assert_eq!(read(&two), Err(ManifestError::Duplicate));
        let big = vec![0u8; MAX_MODULE_BYTES + 1];
        assert_eq!(read(&big), Err(ManifestError::TooLarge(big.len())));
        let huge = module(&[manifest(&"#".repeat(MAX_MANIFEST_BYTES + 1))]);
        assert!(matches!(read(&huge), Err(ManifestError::Malformed(_))));
    }

    #[tuff_core::watchdog]
    #[test]
    fn truncated_and_hostile_sections_fail_cleanly() {
        let whole = module(&[manifest("id=a\nversion=1.0.0\napi = 0")]);
        for len in 9..whole.len() {
            let result = read(&whole[..len]);
            assert!(
                matches!(
                    result,
                    Err(ManifestError::Truncated) | Err(ManifestError::Missing)
                ),
                "{len}: {result:?}"
            );
        }
        let mut claims_4gib = EMPTY.to_vec();
        claims_4gib.extend_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0x0f]);
        assert_eq!(read(&claims_4gib), Err(ManifestError::Truncated));
        let mut endless = EMPTY.to_vec();
        endless.extend_from_slice(&[0, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01]);
        assert!(matches!(read(&endless), Err(ManifestError::Malformed(_))));
        let not_utf8 = module(&[custom_section(SECTION, &[0xff, 0xfe])]);
        assert!(matches!(read(&not_utf8), Err(ManifestError::Malformed(_))));
    }

    #[tuff_core::watchdog]
    #[test]
    fn rejects_bad_fields() {
        for text in [
            "version=1.0.0\napi = 0",
            "id=a\napi = 0",
            "id=a\nversion=1.0.0",
            "id=Bad Id\nversion=1.0.0\napi = 0",
            "id=a\nversion=1.0\napi = 0",
            "id=a\nversion=1.0.0.0\napi = 0",
            "id=a\nversion=1.0.0\napi=one",
            "id=a\nversion=1.0.0\napi = 0\ndepends=a",
            "id=a\nversion=1.0.0\napi = 0\ndepends=b >= x",
            "id=a\nversion=1.0.0\napi = 0\nsoft-depends=a",
            "id=a\nversion=1.0.0\napi = 0\nconflicts=a",
            "id=a\nversion=1.0.0\napi = 0\nload-before=a",
            "id=a\nversion=1.0.0\napi = 0\ndepends=b\nconflicts=b",
            "id=a\nversion=1.0.0\napi = 0\nsoft-depends=b\nconflicts=b >= 1.0.0",
            "id=a\nversion=1.0.0\napi = 0\nload-before=b >= 1.0.0",
            "id=a\nversion=1.0.0\napi = 0\nno equals",
            "id=a\nversion=1.0.0\napi = 0\ncap.tcp = no-port.example",
            "id=a\nversion=1.0.0\napi = 0\ncap.ftp = files.example",
        ] {
            assert!(
                matches!(parse(text), Err(ManifestError::Malformed(_))),
                "{text:?}"
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn reads_optional_keys() {
        let manifest = parse(
            "id=a\nversion=1.0.0\napi = 0\nsoft-depends = bots, economy >= 2.0.0\n\
             conflicts = legacy >= 1.5.0\nload-before = x, y\n",
        )
        .unwrap();
        assert_eq!(manifest.soft_depends.len(), 2);
        assert!(!manifest.soft_depends[1].accepts(Version::new(1, 0, 0)));
        assert!(manifest.soft_depends[1].accepts(Version::new(2, 0, 0)));
        assert!(!manifest.conflicts[0].accepts(Version::new(1, 4, 0)));
        assert_eq!(manifest.load_before, ["x", "y"]);
        let many = format!(
            "id=a\nversion=1.0.0\napi = 0\ndepends={}\nsoft-depends={}",
            (0..20)
                .map(|n| format!("d{n}"))
                .collect::<Vec<_>>()
                .join(","),
            (0..20)
                .map(|n| format!("s{n}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(matches!(parse(&many), Err(ManifestError::Malformed(_))));
    }

    #[tuff_core::watchdog]
    #[test]
    fn reads_capability_requests() {
        let manifest = parse(
            "id = relay\nversion = 1.0.0\napi = 0\n\
             cap.http = discord.com/api/webhooks/ -- Relays chat to Discord\n\
             cap.files = 16 MiB\n",
        )
        .unwrap();
        assert_eq!(manifest.capabilities.len(), 2);
        assert_eq!(manifest.capabilities[0].reason, "Relays chat to Discord");
        assert_eq!(manifest.capabilities[1].entry.to_string(), "files 16 MiB");
        let many = format!(
            "id=a\nversion=1.0.0\napi = 0\ncap.route = {}",
            vec!["/x"; caps::policy::MAX_REQUESTS + 1].join(",")
        );
        assert!(matches!(parse(&many), Err(ManifestError::Malformed(_))));
    }
}
