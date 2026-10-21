//! The advancement framework: the [`Advancements`] service (core's [`Tree`] by default), the
//! trigger [`trigger::Event`] vocabulary sessions fire, and the
//! [`Definition`]s features add. Vanilla supplies its tree and trigger handlers.

mod seam;
pub mod tree;
pub mod trigger;
pub use seam::{
    Advancements, NoAdvancements, Overlay, OverlayFile, ProgressView, Related, Reward, View,
};
pub use tree::{
    Granted, Node, Progress, SavedProgress, SessionAdvancements, State, Tree, Update,
    definitions_of, entry, merge,
};

use tuff_vanilla_data::advancement as data;

/// An advancement's index in the service's tree (sorted by name).
pub type Id = u16;

/// An advancement a feature adds beside the data pack's, naming its parent
/// instead of indexing it (indices only exist once the tree is merged).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Definition {
    pub name: &'static str,
    pub parent: Option<&'static str>,
    pub display: Option<data::Display>,
    pub criteria: &'static [data::Criterion],
    pub requirements: &'static [&'static [&'static str]],
    pub rewards: data::Rewards,
    pub sends_telemetry_event: bool,
    pub position: [f32; 2],
}

/// Epoch milliseconds now.
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
