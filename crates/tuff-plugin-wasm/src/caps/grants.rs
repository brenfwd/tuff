//! The owner's grants, kept in the server's SQLite file (never in a file
//! the owner or a plugin edits by hand) and changed in game with
//! `/tuff plugin grant|revoke`. Nothing is granted unless recorded here.
//!
//! A grant is stored as a text body, one line per entry:
//!
//! ```text
//! http = discord.com/api/webhooks/
//! route = /dynmap top
//! files = 16 MiB
//! limit.requests-per-minute = 300
//! ```

use std::collections::BTreeMap;

use super::pattern::{Entry, Kind};
use super::quota::Limits;
#[cfg(test)]
use crate::manifest;

const MAX_ENTRIES: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grant {
    pub entries: Vec<Entry>,
    pub limits: Limits,
}

impl Grant {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.limits == Limits::default()
    }

    /// Adds `entry`; false if it was already granted or the grant is full.
    pub fn add(&mut self, entry: Entry) -> bool {
        if self.entries.contains(&entry) || self.entries.len() >= MAX_ENTRIES {
            return false;
        }
        self.entries.push(entry);
        true
    }

    /// Removes the entry equal to `entry`; false if it was not granted.
    pub fn remove(&mut self, entry: &Entry) -> bool {
        let before = self.entries.len();
        self.entries.retain(|have| have != entry);
        self.entries.len() != before
    }

    /// The text form kept in storage.
    pub fn render(&self) -> String {
        let mut text = String::new();
        for entry in &self.entries {
            let line = entry.to_string();
            let (kind, rest) = line.split_once(' ').unwrap_or((&line, ""));
            text.push_str(&format!("{kind} = {rest}\n"));
        }
        let defaults = Limits::default();
        for name in Limits::NAMES {
            let value = self.limits.get(name);
            if value != defaults.get(name) {
                text.push_str(&format!("limit.{name} = {value}\n"));
            }
        }
        text
    }

    pub fn parse(body: &str) -> Result<Grant, String> {
        let mut grant = Grant::default();
        for (number, line) in body.lines().enumerate() {
            let at = |why: String| format!("line {}: {why}", number + 1);
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| at(format!("{line:?} is not key = value")))?;
            let (key, value) = (key.trim(), value.trim());
            if let Some(name) = key.strip_prefix("limit.") {
                grant.limits.set(name, value).map_err(at)?;
            } else {
                let kind = Kind::from_name(key)
                    .ok_or_else(|| at(format!("{key:?} is not a capability or limit")))?;
                for entry in Entry::parse_list(kind, value).map_err(at)? {
                    if !grant.add(entry) && grant.entries.len() >= MAX_ENTRIES {
                        return Err(at(format!("over {MAX_ENTRIES} entries")));
                    }
                }
            }
        }
        Ok(grant)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    plugins: BTreeMap<String, Grant>,
}

impl Grants {
    pub fn get(&self, id: &str) -> Option<&Grant> {
        self.plugins.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.plugins.keys().map(String::as_str)
    }

    /// Replaces the grant for `id`; an empty grant removes it.
    pub fn set(&mut self, id: &str, grant: Grant) {
        if grant.is_empty() {
            self.plugins.remove(id);
        } else {
            self.plugins.insert(id.to_owned(), grant);
        }
    }

    /// Reads stored `(plugin id, body)` rows; a row that does not parse
    /// grants nothing and is reported.
    pub fn from_rows(rows: Vec<(String, String)>) -> (Grants, Vec<String>) {
        let mut grants = Grants::default();
        let mut errors = Vec::new();
        for (id, body) in rows {
            match Grant::parse(&body) {
                Ok(grant) => grants.set(&id, grant),
                Err(err) => errors.push(format!("stored grant for {id}: {err}")),
            }
        }
        (grants, errors)
    }

    /// Parses `[plugin-id]` sections of grant bodies, as test fixtures.
    #[cfg(test)]
    pub fn parse(text: &str) -> Result<Grants, String> {
        let mut bodies: BTreeMap<String, String> = BTreeMap::new();
        let mut current: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(id) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                if !manifest::valid_id(id.trim()) {
                    return Err(format!("{id:?} is not a plugin id"));
                }
                current = Some(id.trim().to_owned());
                bodies.entry(id.trim().to_owned()).or_default();
            } else {
                let id = current.as_ref().ok_or("a grant before any [plugin-id]")?;
                let body = bodies.get_mut(id).expect("section exists");
                body.push_str(line);
                body.push('\n');
            }
        }
        let mut grants = Grants::default();
        for (id, body) in bodies {
            grants.set(&id, Grant::parse(&body)?);
        }
        Ok(grants)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::pattern::Rule;

    #[tuff_core::watchdog]
    #[test]
    fn bodies_round_trip_through_render() {
        let body = "http = discord.com/api/webhooks/, *.discordapp.com\n\
                    tcp = 127.0.0.1:5432 private\n\
                    route = /dynmap top\n\
                    route = /map\n\
                    listen-udp = 19132 bind=::1\n\
                    files = 16 MiB\n\
                    limit.requests-per-minute = 300\n\
                    limit.route-timeout = 5s\n";
        let grant = Grant::parse(body).unwrap();
        assert_eq!(grant.entries.len(), 7);
        assert_eq!(grant.entries[4].rule, Rule::Route("/map".into()));
        assert_eq!(grant.entries[3].rule, Rule::TopRoute("/dynmap".into()));
        assert_eq!(grant.entries[6].rule, Rule::Files(16 << 20));
        assert_eq!(grant.limits.requests_per_minute, 300);
        assert_eq!(Grant::parse(&grant.render()).unwrap(), grant);
    }

    #[tuff_core::watchdog]
    #[test]
    fn mistakes_fail_the_body_with_a_line_number() {
        for (text, line) in [
            ("htp = a.example", 1),
            ("\nlimit.conections = 3", 2),
            ("tcp = a.example", 1),
            ("just words", 1),
            ("route = /a top extra", 1),
        ] {
            let err = Grant::parse(text).unwrap_err();
            assert!(err.starts_with(&format!("line {line}:")), "{text:?}: {err}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn empty_grants_are_dropped_and_bad_rows_grant_nothing() {
        let (grants, errors) = Grants::from_rows(vec![
            ("a".into(), "route = /map\n".into()),
            ("b".into(), "".into()),
            ("c".into(), "nonsense".into()),
        ]);
        assert_eq!(grants.ids().collect::<Vec<_>>(), ["a"]);
        assert_eq!(errors.len(), 1);
    }
}
