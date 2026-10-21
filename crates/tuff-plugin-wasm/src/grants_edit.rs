//! Owner edits of plugin grants, shared by `/tuff plugin grant|revoke|grants`
//! and the offline `tuff plugin` subcommand, so both validate and store alike.

use std::path::Path;

use tuff_storage::config::ConfigStore;

use crate::caps::{Entry, Grant, Grants, Kind, Limits as CapLimits, Policy, Rule};
use crate::manifest::{self, Manifest};

/// An owner's edit of one plugin's grants. `cap` is a capability kind
/// (`http`, `route`, ...) or `limit.<name>`; `entry` is entry text, or
/// `all-requested` (grant) or `all` (revoke).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantChange {
    Grant { cap: String, entry: String },
    Revoke { cap: String, entry: String },
}

/// What a plugin requests and what the owner has granted it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrantReport {
    /// Requested entries as `(text, reason, granted)`.
    pub requested: Vec<(String, String, bool)>,
    /// Granted entries and limits the plugin did not request.
    pub extra: Vec<String>,
    pub notes: Vec<String>,
}

pub(crate) fn report(manifest: &Manifest, grant: &Grant, policy: &Policy) -> GrantReport {
    let requested = manifest
        .capabilities
        .iter()
        .map(|request| {
            let granted = grant.entries.iter().any(|g| g.covers(&request.entry));
            (request.entry.to_string(), request.reason.clone(), granted)
        })
        .collect();
    let mut extra: Vec<String> = grant
        .entries
        .iter()
        .filter(|g| {
            !manifest
                .capabilities
                .iter()
                .any(|r| g.covers(&r.entry) || r.entry.covers(g))
        })
        .map(Entry::to_string)
        .collect();
    let defaults = CapLimits::default();
    for name in CapLimits::NAMES {
        if grant.limits.get(name) != defaults.get(name) {
            extra.push(format!("limit.{name} = {}", grant.limits.get(name)));
        }
    }
    GrantReport {
        requested,
        extra,
        notes: policy.report(),
    }
}

/// Finds the manifest of plugin `id` among the modules in `directory`.
pub fn find_manifest(directory: &Path, id: &str) -> Result<Manifest, String> {
    let entries = std::fs::read_dir(directory).map_err(|err| {
        format!(
            "cannot read plugin directory {}: {err}",
            directory.display()
        )
    })?;
    let mut files: Vec<_> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "wasm")
                && !path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    files.sort();
    for path in files {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Ok(found) = manifest::read(&bytes)
            && found.id == id
        {
            return Ok(found);
        }
    }
    Err(format!("no plugin {id} in {}", directory.display()))
}

fn stored(store: &dyn ConfigStore, id: &str) -> Result<Grant, String> {
    let rows = store
        .read_plugin_grants()
        .map_err(|err| format!("cannot read the grants: {err}"))?;
    let (grants, errors) = Grants::from_rows(rows);
    if let Some(error) = errors.into_iter().find(|e| e.contains(id)) {
        return Err(error);
    }
    Ok(grants.get(id).cloned().unwrap_or_default())
}

/// Applies `change` to the stored grant of plugin `id` (manifest read from
/// `directory`) without a running server.
pub fn change_offline(
    store: &dyn ConfigStore,
    directory: &Path,
    id: &str,
    change: GrantChange,
) -> Result<String, String> {
    let manifest = find_manifest(directory, id)?;
    let mut grant = stored(store, id)?;
    let message = apply_change(&mut grant, &manifest, change)?;
    if let Some(path) = grant.entries.iter().find_map(|e| match &e.rule {
        Rule::TopRoute(path) if !servable_top_route(path) => Some(path),
        _ => None,
    }) {
        return Err(format!("cannot grant the route: {path} is reserved"));
    }
    let body = (!grant.is_empty()).then(|| grant.render());
    store
        .write_plugin_grant(id, body.as_deref())
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::Unsupported => {
                "this storage backend cannot hold plugin grants; use a sqlite server file"
                    .to_owned()
            }
            _ => format!("cannot save the grant: {err}"),
        })?;
    Ok(message)
}

fn servable_top_route(path: &str) -> bool {
    let trimmed = path.trim_matches('/');
    !trimmed.is_empty()
        && trimmed != crate::routes::PREFIX.trim_matches('/')
        && !path.starts_with(&format!("{}/", crate::routes::PREFIX))
}

fn same_ignoring_private(have: &Entry, wanted: &Entry) -> bool {
    match (&have.rule, &wanted.rule) {
        (Rule::Net(a), Rule::Net(b)) if have.kind == wanted.kind => {
            let mut a = a.clone();
            a.private = b.private;
            a == *b
        }
        _ => have == wanted,
    }
}

/// What plugin `id` requests and what is granted, without a running server.
pub fn report_offline(
    store: &dyn ConfigStore,
    directory: &Path,
    id: &str,
) -> Result<GrantReport, String> {
    let manifest = find_manifest(directory, id)?;
    let grant = stored(store, id)?;
    let policy = Policy::new(manifest.capabilities.clone(), Some(&grant));
    Ok(report(&manifest, &grant, &policy))
}

/// Applies one owner edit to `grant`, returning what was done.
pub(crate) fn apply_change(
    grant: &mut Grant,
    manifest: &Manifest,
    change: GrantChange,
) -> Result<String, String> {
    let (revoke, cap, entry) = match change {
        GrantChange::Grant { cap, entry } => (false, cap, entry),
        GrantChange::Revoke { cap, entry } => (true, cap, entry),
    };
    let entry = entry.trim();
    if let Some(name) = cap.strip_prefix("limit.") {
        let value = match revoke {
            true => CapLimits::default().get(name),
            false => entry.to_owned(),
        };
        grant.limits.set(name, &value)?;
        return Ok(format!("{cap} is now {}", grant.limits.get(name)));
    }
    let kind = Kind::from_name(&cap).ok_or_else(|| format!("{cap:?} is not a capability"))?;
    if revoke {
        if entry == "all" {
            let before = grant.entries.len();
            grant.entries.retain(|e| e.kind != kind);
            return Ok(format!(
                "Revoked {} {cap} entries",
                before - grant.entries.len()
            ));
        }
        let wanted = Entry::parse(kind, entry)?;
        let removed: Vec<String> = grant
            .entries
            .iter()
            .filter(|have| same_ignoring_private(have, &wanted))
            .map(ToString::to_string)
            .collect();
        grant
            .entries
            .retain(|have| !same_ignoring_private(have, &wanted));
        return match removed.is_empty() {
            false => Ok(format!("Revoked {}", removed.join(", "))),
            true => Err(format!("{wanted} is not granted")),
        };
    }
    if entry != "all-requested" {
        let wanted = Entry::parse(kind, entry)?;
        return match grant.add(wanted.clone()) {
            true => Ok(format!("Granted {wanted}")),
            false => Err(format!("{wanted} is already granted")),
        };
    }
    let (mut added, mut skipped) = (0, Vec::new());
    for request in manifest
        .capabilities
        .iter()
        .filter(|r| r.entry.kind == kind)
    {
        let sensitive = match &request.entry.rule {
            Rule::Net(rule) => rule.private,
            Rule::TopRoute(_) => true,
            _ => false,
        };
        if sensitive {
            skipped.push(request.entry.to_string());
        } else if grant.add(request.entry.clone()) {
            added += 1;
        }
    }
    let mut done = format!("Granted {added} requested {cap} entries");
    if !skipped.is_empty() {
        done.push_str(&format!("; grant these explicitly: {}", skipped.join(", ")));
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        manifest::parse("id = x\nversion = 1.0.0\napi = 1\n").unwrap()
    }

    fn change(revoke: bool, cap: &str, entry: &str) -> GrantChange {
        let (cap, entry) = (cap.to_owned(), entry.to_owned());
        match revoke {
            true => GrantChange::Revoke { cap, entry },
            false => GrantChange::Grant { cap, entry },
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn revoke_ignores_the_private_flag() {
        let mut grant = Grant::default();
        apply_change(
            &mut grant,
            &manifest(),
            change(false, "tcp", "10.0.0.1:5432 private"),
        )
        .unwrap();
        let done = apply_change(
            &mut grant,
            &manifest(),
            change(true, "tcp", "10.0.0.1:5432"),
        )
        .unwrap();
        assert!(done.starts_with("Revoked"), "{done}");
        assert!(grant.is_empty());
        assert!(
            apply_change(
                &mut grant,
                &manifest(),
                change(true, "tcp", "10.0.0.1:5432")
            )
            .is_err()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn reserved_top_routes_are_not_servable() {
        assert!(!servable_top_route("/plugins"));
        assert!(!servable_top_route("/plugins/x"));
        assert!(!servable_top_route("/"));
        assert!(servable_top_route("/dynmap"));
    }
}
