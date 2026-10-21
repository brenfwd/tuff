//! The opt-in shared playhead: one leader publishes its playback state and
//! followers copy it, each seeking its own transformer.

use std::sync::Arc;

use tokio::sync::watch;

/// What a follower copies from the leader.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LeaderState {
    pub leader: Option<i32>,
    pub position: i64,
    pub speed: f64,
    pub paused: bool,
    /// Counts the leader's seeks, so a follower seeks too rather than
    /// playing the gap.
    pub seeks: u64,
}

pub type Shared = Arc<watch::Sender<LeaderState>>;

pub fn new() -> Shared {
    Arc::new(watch::channel(LeaderState::default()).0)
}
