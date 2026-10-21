//! Built-in content hidden by the `filter` of packs above `vanilla`.

use std::collections::BTreeMap;

use tuff_datapack::tags;

use super::{TAGS, hidden, meta};

/// The path a resource `id` of a kind directory (`recipe/`) has below its
/// namespace, with `extension`.
fn resource_path<'a>(kind: &str, id: &'a str, extension: &str) -> (&'a str, String) {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    (namespace, format!("{kind}{path}{extension}"))
}

/// Built-in content the packs above `vanilla` hide.
#[derive(Default)]
pub(super) struct HiddenBuiltIn<'a> {
    pub(super) hiders: Vec<&'a [meta::FilterPattern]>,
}

impl HiddenBuiltIn<'_> {
    pub(super) fn hides(&self, kind: &str, id: &str, extension: &str) -> bool {
        if self.hiders.is_empty() {
            return false;
        }
        let (namespace, path) = resource_path(kind, id, extension);
        hidden(&self.hiders, namespace, &path)
    }

    /// Drops the built-in members of every tag it hides; true if any was.
    pub(super) fn strip_tags(&self, builders: &mut BTreeMap<&'static str, tags::Builders>) -> bool {
        let mut any = false;
        for (registry, tags) in builders.iter_mut() {
            let directory = registry.strip_prefix("minecraft:").unwrap_or(registry);
            let kind = format!("{TAGS}{directory}/");
            for (tag, entries) in tags.iter_mut() {
                if !entries.is_empty() && self.hides(&kind, tag, ".json") {
                    entries.clear();
                    any = true;
                }
            }
        }
        any
    }
}
