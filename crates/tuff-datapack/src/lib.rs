//! Pure data pack pieces: zip archives, tag sets, loot tables and recipes.

pub mod loot;
pub mod recipe;
pub mod slot_source;
pub mod tags;
pub mod zip;

pub fn valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace != ".."
        && namespace
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'))
}

pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '/')
        })
        && path
            .split('/')
            .all(|segment| segment != "." && segment != "..")
}

/// `text` as a full `namespace:path` id, if it is a valid one.
pub fn identifier(text: &str) -> Option<String> {
    let (namespace, path) = text.split_once(':').unwrap_or(("minecraft", text));
    let namespace = if namespace.is_empty() {
        "minecraft"
    } else {
        namespace
    };
    (valid_namespace(namespace) && valid_path(path)).then(|| format!("{namespace}:{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn dot_dot_is_not_a_namespace() {
        assert!(!valid_namespace(".."));
        assert!(valid_namespace("my.pack"));
        assert!(identifier("..:stone").is_none());
    }
}
