pub mod trigger;

use std::sync::Arc;

use tuff_core::world::advancement::Tree;

pub fn builtin() -> Tree {
    Tree::new(
        &tuff_vanilla_data::advancement::ALL,
        trigger::builtin_registry(),
    )
}

/// One vanilla tree shared by every caller that needs no extensions
/// (tests start many worlds).
pub fn shared_builtin() -> Arc<Tree> {
    static TREE: std::sync::OnceLock<Arc<Tree>> = std::sync::OnceLock::new();
    TREE.get_or_init(|| Arc::new(builtin())).clone()
}

#[cfg(test)]
mod tests;
