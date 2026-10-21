use super::*;

/// The channels this session is driven by and sends through.
pub struct Conn {
    pub outbound: mpsc::UnboundedSender<Bytes>,
    pub handle: PlayerHandle,
    pub events: mpsc::UnboundedReceiver<PlayerEvent>,
    pub packets: mpsc::Receiver<Serverbound>,
    pub clock: watch::Receiver<u64>,
    pub server_ticks: watch::Receiver<u64>,
}
