use super::*;

/// Ticking clocks, shutdown state and background job bookkeeping.
pub(super) struct Sched {
    pub(super) clock: watch::Receiver<u64>,
    pub(super) server_ticks: watch::Receiver<u64>,
    /// The server tick last seen; chunk keep-alive and saving count these,
    /// so they go on while the game is frozen.
    pub(super) server_tick: u64,
    /// This tick is a frozen one: only what players ride moves.
    pub(super) frozen: bool,
    pub(super) tick: u64,
    /// The last tick `on_tick` ran.
    pub(super) ticked: u64,
    /// Released when the region starts closing and stops ticking.
    pub(super) tick_member: Option<TickMember>,
    pub(super) beat: Option<Arc<crate::world::stall::Beat>>,
    pub(super) closing: Option<watch::Sender<bool>>,
    pub(super) final_save: bool,
    pub(super) requests_open: bool,
    pub(super) internal_open: bool,
    pub(super) jobs_tx: mpsc::UnboundedSender<JobResult>,
    pub(super) jobs: mpsc::UnboundedReceiver<JobResult>,
    pub(super) jobs_in_flight: usize,
    /// `Settled` requests waiting for their tick or for jobs to land.
    pub(super) settle_waiters: Vec<(u64, tokio::sync::oneshot::Sender<()>)>,
    /// Flushing saves waiting for every chunk save under way to land.
    pub(super) save_waiters: Vec<tokio::sync::oneshot::Sender<()>>,
}

impl Sched {
    pub(super) fn new(shared: &Shared) -> Self {
        let (jobs_tx, jobs) = mpsc::unbounded_channel();
        let mut clock = shared.clock.clone();
        if shared.tick_state.borrow().frozen {
            clock.borrow_and_update();
        }
        let server_ticks = shared.server_ticks.clone();
        let server_tick = *server_ticks.borrow();
        let tick = *clock.borrow();
        Self {
            clock,
            server_ticks,
            server_tick,
            frozen: false,
            tick,
            ticked: 0,
            tick_member: Some(shared.tick_barrier.join(tick)),
            beat: None,
            closing: None,
            final_save: false,
            requests_open: true,
            internal_open: true,
            jobs_tx,
            jobs,
            jobs_in_flight: 0,
            settle_waiters: Vec::new(),
            save_waiters: Vec::new(),
        }
    }
}
