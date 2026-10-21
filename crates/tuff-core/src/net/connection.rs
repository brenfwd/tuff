use std::io::IoSlice;

use anyhow::{Result, bail};
use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use crate::metrics::{Metrics, NetMeter, StateGauge};
use crate::net::crypto::{Decryptor, Encryptor, ProtocolCipher, SHARED_SECRET_LEN};
use crate::protocol::{Encode, Packet, encode_packet};
use std::sync::Arc;
use tuff_trace::{ConnState, ConnTrace, Tracer};
use tuff_wire::{Buffer, PacketReader};

const READ_CHUNK: usize = 4096;

/// Frames an encoded packet for the wire, compressing it when compression is on.
pub fn frame<P: Encode>(packet: &P, compression: Option<usize>) -> Bytes {
    let _span = tuff_timings::span!(
        "net.encode",
        tuff_timings::Tag::Name(std::any::type_name::<P>())
    );
    let writer = encode_packet(packet);
    if writer.len() > MAX_OUTBOUND_UNCOMPRESSED {
        log::warn!(
            "dropping oversize clientbound packet {} ({} bytes)",
            std::any::type_name::<P>(),
            writer.len()
        );
        return Bytes::new();
    }
    let framed = match compression {
        None => writer.finish(),
        Some(threshold) => writer
            .finish_compressed(threshold)
            .expect("in-memory zlib compression is infallible"),
    };
    if !length_prefix_fits(&framed) {
        log::warn!(
            "dropping clientbound packet {} whose frame exceeds the three byte length prefix",
            std::any::type_name::<P>()
        );
        return Bytes::new();
    }
    framed
}

/// Largest uncompressed packet body the client accepts.
const MAX_OUTBOUND_UNCOMPRESSED: usize = 8_388_608;

fn length_prefix_fits(framed: &[u8]) -> bool {
    framed.iter().take(3).any(|b| b & 0x80 == 0)
}

/// A framed, optionally encrypted and compressed Minecraft protocol stream.
/// Owned by exactly one task until it is split into its two halves.
pub struct Connection {
    reader: ConnectionReader,
    writer: ConnectionWriter,
    trace: Option<Arc<ConnTrace>>,
}

impl Connection {
    pub fn new(socket: TcpStream) -> Self {
        let (read, write) = socket.into_split();
        Self {
            reader: ConnectionReader {
                socket: read,
                buffer: Buffer::new(),
                decryptor: None,
                trace: None,
                bytes_read: 0,
                meter: None,
                state: None,
            },
            writer: ConnectionWriter {
                socket: write,
                encryptor: None,
                compression: None,
                trace: None,
                meter: None,
            },
            trace: None,
        }
    }

    /// Reports this connection's packets to the tracer once a player name is
    /// set with [`Connection::trace_player`].
    /// Bounds frames while the peer is unauthenticated; `None` lifts it.
    pub fn set_frame_limit(&mut self, limit: Option<usize>) {
        self.reader.buffer.set_packet_limit(limit);
    }

    pub fn set_tracer(&mut self, tracer: Tracer) {
        let trace = ConnTrace::new(tracer);
        self.reader.trace = Some(trace.clone());
        self.writer.trace = Some(trace.clone());
        self.trace = Some(trace);
    }

    /// Counts this connection's traffic and protocol state in `metrics`.
    pub fn set_metrics(&mut self, metrics: &Metrics) {
        self.reader.meter = Some(NetMeter::inbound(metrics));
        self.reader.state = Some(StateGauge::new(metrics, ConnState::Handshake));
        self.writer.meter = Some(NetMeter::outbound(metrics));
    }

    /// Records the protocol state for the tracer and the metrics.
    pub fn trace_state(&mut self, state: ConnState) {
        if let Some(trace) = &self.trace {
            trace.set_state(state);
        }
        if let Some(gauge) = &mut self.reader.state {
            gauge.set(state);
        }
    }

    pub fn trace_player(&self, name: &str) {
        if let Some(trace) = &self.trace {
            trace.set_player(name);
        }
    }

    /// Enables AES/CFB8 for all subsequent reads and writes.
    pub fn enable_encryption(&mut self, shared_secret: &[u8; SHARED_SECRET_LEN]) {
        let (encryptor, decryptor) = ProtocolCipher::new(shared_secret).split();
        self.reader.decryptor = Some(decryptor);
        self.writer.encryptor = Some(encryptor);
    }

    /// Enables compression for all subsequent reads and writes. The caller
    /// must already have sent Set Compression.
    pub fn enable_compression(&mut self, threshold: usize) {
        self.reader.buffer.set_compression_threshold(threshold);
        self.writer.compression = Some(threshold);
    }

    pub fn compression(&self) -> Option<usize> {
        self.writer.compression
    }

    pub async fn read_packet(&mut self) -> Result<Option<Bytes>> {
        self.reader.read_packet().await
    }

    pub async fn expect<P: Packet>(&mut self) -> Result<P> {
        self.reader.expect().await
    }

    pub async fn send_raw(&mut self, framed: &Bytes) -> Result<()> {
        self.writer.send_raw(framed).await
    }

    pub async fn send<P: Encode>(&mut self, packet: &P) -> Result<()> {
        self.writer.send(packet).await
    }

    pub fn split(self) -> (ConnectionReader, ConnectionWriter) {
        (self.reader, self.writer)
    }

    /// Puts halves from [`Connection::split`] back together.
    pub fn rejoin(reader: ConnectionReader, writer: ConnectionWriter) -> Self {
        let trace = reader.trace.clone();
        Self {
            reader,
            writer,
            trace,
        }
    }

    /// Flushes and closes the write half of the socket.
    pub async fn close(self) -> Result<()> {
        self.writer.close().await
    }
}

pub struct ConnectionReader {
    socket: OwnedReadHalf,
    buffer: Buffer,
    decryptor: Option<Decryptor>,
    trace: Option<Arc<ConnTrace>>,
    bytes_read: u64,
    meter: Option<NetMeter>,
    /// Lives as long as the read half, which outlives every state.
    state: Option<StateGauge>,
}

impl ConnectionReader {
    /// Bytes read off the socket so far, as they travelled on the wire.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// The framed size on the wire of the packet read last.
    pub fn last_frame_len(&self) -> usize {
        self.buffer.last_frame_len()
    }

    /// Reads the next packet, returning `None` on a clean EOF.
    pub async fn read_packet(&mut self) -> Result<Option<Bytes>> {
        let mut chunk = [0u8; READ_CHUNK];
        loop {
            if let Some(payload) = self.buffer.try_split_packet()? {
                if let Some(meter) = &mut self.meter {
                    meter.packets(1);
                }
                if let Some(trace) = &self.trace {
                    trace.serverbound(&payload);
                }
                return Ok(Some(payload));
            }
            let n = self.socket.read(&mut chunk).await?;
            if n == 0 {
                return Ok(None);
            }
            self.bytes_read += n as u64;
            if let Some(meter) = &mut self.meter {
                meter.bytes(n);
            }
            if let Some(decryptor) = &mut self.decryptor {
                decryptor.decrypt_in_place(&mut chunk[..n]);
            }
            self.buffer.extend(&chunk[..n])?;
        }
    }

    /// Reads the next packet and decodes it as `P`, failing on EOF or a different packet ID.
    pub async fn expect<P: Packet>(&mut self) -> Result<P> {
        let payload = self
            .read_packet()
            .await?
            .ok_or_else(|| anyhow::anyhow!("connection closed"))?;
        let mut reader = PacketReader::new(payload);
        let id = reader.read_var_int()?;
        anyhow::ensure!(id == P::ID, "expected packet {:#04x}, got {id:#04x}", P::ID);
        let packet = P::decode(&mut reader)?;
        anyhow::ensure!(
            reader.remaining() == 0,
            "trailing bytes in packet {id:#04x}"
        );
        Ok(packet)
    }
}

pub enum Pumped {
    Drained,
    /// The stop signal fired with nothing left queued.
    Stopped,
    Failed,
    Overflow(usize),
}

pub struct ConnectionWriter {
    socket: OwnedWriteHalf,
    encryptor: Option<Encryptor>,
    compression: Option<usize>,
    trace: Option<Arc<ConnTrace>>,
    meter: Option<NetMeter>,
}

impl ConnectionWriter {
    fn meter(&mut self, frames: &[Bytes]) {
        if let Some(meter) = &mut self.meter {
            meter.bytes(frames.iter().map(Bytes::len).sum());
            meter.packets(frames.len());
        }
    }

    /// Writes an already framed packet. Zero-copy unless encryption is enabled.
    pub async fn send_raw(&mut self, framed: &Bytes) -> Result<()> {
        if let Some(trace) = &self.trace {
            trace.clientbound_frame(framed, self.compression);
        }
        self.meter(std::slice::from_ref(framed));
        match &mut self.encryptor {
            None => self.socket.write_all(framed).await?,
            Some(encryptor) => {
                let mut encrypted = BytesMut::from(&framed[..]);
                encryptor.encrypt_in_place(&mut encrypted);
                self.socket.write_all(&encrypted).await?;
            }
        }
        Ok(())
    }

    /// Writes framed packets in order with as few socket writes as possible:
    /// one vectored write without encryption (zero-copy), else one buffer
    /// encrypted in a single pass.
    pub async fn pump_outbound(
        &mut self,
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<Bytes>,
        batch_size: usize,
        limit: usize,
    ) -> Pumped {
        self.pump_outbound_until(rx, batch_size, limit, std::future::pending())
            .await
    }

    /// [`ConnectionWriter::pump_outbound`] that also returns once `stop` has
    /// resolved and everything queued before it has been written.
    pub async fn pump_outbound_until(
        &mut self,
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<Bytes>,
        batch_size: usize,
        limit: usize,
        stop: impl std::future::Future<Output = ()>,
    ) -> Pumped {
        let mut stop = std::pin::pin!(stop);
        let mut batch = Vec::with_capacity(batch_size);
        let mut pending = Vec::new();
        let mut closed = false;
        loop {
            if pending.is_empty() {
                if closed {
                    return Pumped::Drained;
                }
                let received = tokio::select! {
                    biased;
                    received = rx.recv_many(&mut batch, batch_size) => received,
                    () = &mut stop => return Pumped::Stopped,
                };
                if received == 0 {
                    return Pumped::Drained;
                }
            } else {
                std::mem::swap(&mut batch, &mut pending);
            }
            {
                let mut write = std::pin::pin!(self.send_batch(&batch));
                loop {
                    tokio::select! {
                        result = &mut write => {
                            if result.is_err() {
                                return Pumped::Failed;
                            }
                            break;
                        }
                        received = rx.recv_many(&mut pending, batch_size), if !closed => {
                            if received == 0 {
                                closed = true;
                                continue;
                            }
                            let queued: usize = batch.iter().chain(&pending).map(Bytes::len).sum();
                            if queued > limit {
                                return Pumped::Overflow(queued);
                            }
                        }
                    }
                }
            }
            batch.clear();
        }
    }

    pub async fn send_batch(&mut self, frames: &[Bytes]) -> Result<()> {
        if let [frame] = frames {
            return self.send_raw(frame).await;
        }
        self.trace_frames(frames);
        self.meter(frames);
        match &mut self.encryptor {
            None => {
                let mut slices: Vec<IoSlice> = frames.iter().map(|f| IoSlice::new(f)).collect();
                let mut remaining = &mut slices[..];
                while !remaining.is_empty() {
                    let written = self.socket.write_vectored(remaining).await?;
                    if written == 0 {
                        bail!("socket closed while writing");
                    }
                    IoSlice::advance_slices(&mut remaining, written);
                }
            }
            Some(encryptor) => {
                let mut buffer = BytesMut::with_capacity(frames.iter().map(Bytes::len).sum());
                for frame in frames {
                    buffer.extend_from_slice(frame);
                }
                encryptor.encrypt_in_place(&mut buffer);
                self.socket.write_all(&buffer).await?;
            }
        }
        Ok(())
    }

    fn trace_frames(&self, frames: &[Bytes]) {
        if let Some(trace) = &self.trace {
            for frame in frames {
                trace.clientbound_frame(frame, self.compression);
            }
        }
    }

    pub async fn send<P: Encode>(&mut self, packet: &P) -> Result<()> {
        self.send_raw(&frame(packet, self.compression)).await
    }

    /// Flushes and closes the write half of the socket.
    pub async fn close(mut self) -> Result<()> {
        self.socket.shutdown().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::play;

    #[tuff_core::watchdog]
    #[test]
    fn oversize_clientbound_packets_are_dropped_not_framed() {
        let motd = tuff_item::text::TextComponent::new("m");
        let big = vec![0u8; 9_000_000];
        let packet = play::ServerData {
            motd: &motd,
            icon: Some(&big),
        };
        assert!(frame(&packet, None).is_empty());
        assert!(frame(&packet, Some(256)).is_empty());
        let mid = vec![0u8; 3_000_000];
        let packet = play::ServerData {
            motd: &motd,
            icon: Some(&mid),
        };
        assert!(frame(&packet, None).is_empty());
        let small = play::ServerData {
            motd: &motd,
            icon: Some(&[1, 2, 3]),
        };
        assert!(!frame(&small, None).is_empty());
    }
    use tokio::net::TcpListener;

    async fn pair() -> (Connection, Connection) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, accepted) = tokio::join!(TcpStream::connect(address), listener.accept());
        (
            Connection::new(accepted.unwrap().0),
            Connection::new(client.unwrap()),
        )
    }

    fn frames() -> Vec<Bytes> {
        let blocks: Vec<(u16, u32)> = (0..4096).map(|i| (i as u16, 1)).collect();
        (0..300)
            .map(|i| match i % 3 {
                0 => frame(
                    &play::SectionBlocksUpdate {
                        section: [0, i, 0],
                        blocks: &blocks,
                    },
                    None,
                ),
                _ => frame(&play::KeepAlive(i64::from(i)), None),
            })
            .collect()
    }

    async fn batches_arrive_whole_and_in_order(encrypted: bool) {
        let (mut server, mut client) = pair().await;
        if encrypted {
            let secret = [7; SHARED_SECRET_LEN];
            server.enable_encryption(&secret);
            client.enable_encryption(&secret);
        }
        let sent = frames();
        let (_, mut writer) = server.split();
        let expected = sent.clone();
        let writing = tokio::spawn(async move {
            for batch in sent.chunks(64) {
                writer.send_batch(batch).await.unwrap();
            }
        });
        for frame in &expected {
            let mut framed = PacketReader::new(frame.clone());
            framed.read_var_int().unwrap();
            let payload = client.read_packet().await.unwrap().unwrap();
            assert_eq!(payload, framed.read_bytes(framed.remaining()).unwrap());
        }
        writing.await.unwrap();
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_client_that_stops_reading_overflows_the_outbound_cap() {
        let (server, _client) = pair().await;
        let (_, mut writer) = server.split();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let limit = 256 * 1024;
        let chunk = Bytes::from(vec![0u8; 64 * 1024]);
        let pump = tokio::spawn(async move { writer.pump_outbound(&mut rx, 16, limit).await });
        let mut sent = 0;
        while !pump.is_finished() {
            tx.send(chunk.clone()).unwrap();
            sent += chunk.len();
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            assert!(sent < 512 * 1024 * 1024, "never overflowed");
        }
        match pump.await.unwrap() {
            Pumped::Overflow(queued) => {
                assert!(queued > limit && queued <= limit + 16 * chunk.len())
            }
            _ => panic!("expected overflow"),
        }
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn traffic_and_protocol_state_are_metered() {
        use tuff_trace::ConnState;
        let metrics = Metrics::spawn(Default::default());
        let core = metrics.core().clone();
        let (mut server, mut client) = pair().await;
        server.set_metrics(&metrics);
        assert_eq!(core.connection(ConnState::Handshake).get(), 1);
        server.trace_state(ConnState::Login);
        server.trace_state(ConnState::Play);
        let sent: Vec<Bytes> = frames().into_iter().take(10).collect();
        for frame in &sent {
            client.send_raw(frame).await.unwrap();
        }
        for _ in &sent {
            server.read_packet().await.unwrap().unwrap();
        }
        let (reader, mut writer) = server.split();
        writer.send_batch(&sent[..3]).await.unwrap();
        writer.send(&play::KeepAlive(1)).await.unwrap();
        for _ in 0..4 {
            client.read_packet().await.unwrap().unwrap();
        }
        assert_eq!(core.connection(ConnState::Handshake).get(), 0);
        assert_eq!(core.connection(ConnState::Play).get(), 1);
        drop(writer);
        drop(reader);
        let total = |frames: &[Bytes]| frames.iter().map(Bytes::len).sum::<usize>() as i64;
        assert_eq!(core.packets_in.get(), 10);
        assert_eq!(core.bytes_in.get(), total(&sent));
        assert_eq!(core.packets_out.get(), 4);
        assert_eq!(
            core.bytes_out.get(),
            total(&sent[..3]) + frame(&play::KeepAlive(1), None).len() as i64
        );
        assert_eq!(core.connection(ConnState::Play).get(), 0);
        let snapshot = metrics.collect().await;
        assert_eq!(snapshot.series["net.packets_in"].value, 10);
        assert_eq!(snapshot.series["net.connections.play"].value, 0);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn plain_batches_arrive_whole_and_in_order() {
        batches_arrive_whole_and_in_order(false).await;
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn encrypted_batches_arrive_whole_and_in_order() {
        batches_arrive_whole_and_in_order(true).await;
    }
}
