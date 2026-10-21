//! Streaming request and response bodies.

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use hyper::body::{Body as HttpBody, Frame, Incoming, SizeHint};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio::time::Sleep;

use super::Response;

/// Bytes a file body reads per chunk.
pub const FILE_CHUNK: usize = 64 * 1024;
/// Chunks a file body reads ahead of the socket.
pub const FILE_QUEUE: usize = 4;

/// A response body: in memory, produced by a task, or read from a file.
pub enum Body {
    Full(Bytes),
    /// Each received chunk is sent as is; the body ends when every sender
    /// is dropped. The channel's bound is the back-pressure.
    Stream(mpsc::Receiver<Bytes>),
    /// `range` is an inclusive byte range; `None` is the whole file.
    File {
        path: PathBuf,
        range: Option<(u64, u64)>,
        len: u64,
    },
}

impl Body {
    pub fn empty() -> Self {
        Self::Full(Bytes::new())
    }

    /// The bytes of a `Full` body.
    pub fn bytes(&self) -> Option<&Bytes> {
        match self {
            Self::Full(bytes) => Some(bytes),
            _ => None,
        }
    }
}

impl From<Bytes> for Body {
    fn from(bytes: Bytes) -> Self {
        Self::Full(bytes)
    }
}

/// The hyper side of a [`Body`].
pub(super) enum OutBody {
    Full(Option<Bytes>),
    Channel {
        rx: mpsc::Receiver<Bytes>,
        idle: Duration,
        timer: Pin<Box<Sleep>>,
        exact: Option<u64>,
    },
}

impl OutBody {
    pub(super) fn new(body: Body, idle: Duration) -> Self {
        let channel = |rx, exact| OutBody::Channel {
            rx,
            idle,
            timer: Box::pin(tokio::time::sleep(idle)),
            exact,
        };
        match body {
            Body::Full(bytes) => OutBody::Full((!bytes.is_empty()).then_some(bytes)),
            Body::Stream(rx) => channel(rx, None),
            Body::File { path, range, len } => {
                let (start, end) = range.unwrap_or((0, len.saturating_sub(1)));
                let count = if len == 0 { 0 } else { end + 1 - start };
                channel(read_file(path, start, count), Some(count))
            }
        }
    }
}

impl HttpBody for OutBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match self.get_mut() {
            OutBody::Full(bytes) => Poll::Ready(bytes.take().map(|b| Ok(Frame::data(b)))),
            OutBody::Channel {
                rx, idle, timer, ..
            } => match rx.poll_recv(cx) {
                Poll::Ready(Some(chunk)) => {
                    timer.as_mut().reset(tokio::time::Instant::now() + *idle);
                    Poll::Ready(Some(Ok(Frame::data(chunk))))
                }
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => match timer.as_mut().poll(cx) {
                    Poll::Ready(()) => Poll::Ready(Some(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "response body idle",
                    )))),
                    Poll::Pending => Poll::Pending,
                },
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        matches!(self, OutBody::Full(None))
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            OutBody::Full(bytes) => {
                SizeHint::with_exact(bytes.as_ref().map_or(0, |b| b.len() as u64))
            }
            OutBody::Channel {
                exact: Some(len), ..
            } => SizeHint::with_exact(*len),
            OutBody::Channel { .. } => SizeHint::default(),
        }
    }
}

/// Reads `count` bytes of `path` from `start` on the blocking pool, one
/// chunk at a time, never more than [`FILE_QUEUE`] chunks ahead.
fn read_file(path: PathBuf, start: u64, count: u64) -> mpsc::Receiver<Bytes> {
    let (tx, rx) = mpsc::channel(FILE_QUEUE);
    tokio::spawn(async move {
        let opened = tokio::task::spawn_blocking(move || -> io::Result<std::fs::File> {
            use std::io::Seek;
            let mut file = std::fs::File::open(&path)?;
            file.seek(io::SeekFrom::Start(start))?;
            Ok(file)
        })
        .await;
        let Ok(Ok(mut file)) = opened else { return };
        let mut remaining = count;
        while remaining > 0 {
            let want = remaining.min(FILE_CHUNK as u64) as usize;
            let read = tokio::task::spawn_blocking(move || {
                use std::io::Read;
                let mut buf = vec![0; want];
                let mut filled = 0;
                while filled < want {
                    match file.read(&mut buf[filled..]) {
                        Ok(0) => break,
                        Ok(n) => filled += n,
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                        Err(err) => return Err(err),
                    }
                }
                buf.truncate(filled);
                Ok((file, buf))
            })
            .await;
            let Ok(Ok((back, buf))) = read else { return };
            file = back;
            if buf.is_empty() {
                return;
            }
            remaining -= buf.len() as u64;
            if tx.send(Bytes::from(buf)).await.is_err() {
                return;
            }
        }
    });
    rx
}

/// Per-route request body handling.
#[derive(Clone, Copy, Debug, Default)]
pub struct RouteOptions {
    /// Largest accepted body; `None` uses the listener's `max_body`.
    pub max_body: Option<usize>,
    /// Hand the handler a [`BodyStream`] instead of collected bytes. The
    /// listener's request timeout then does not apply to the handler.
    pub stream: bool,
}

/// Why reading a request body stopped.
#[derive(Debug)]
pub enum BodyError {
    TooLarge,
    /// No data within the idle timeout.
    Idle,
    /// Slower than the listener's minimum upload rate.
    TooSlow,
    Closed(String),
}

impl BodyError {
    pub fn response(&self) -> Response {
        use hyper::StatusCode;
        let status = match self {
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Idle | Self::TooSlow => StatusCode::REQUEST_TIMEOUT,
            Self::Closed(_) => StatusCode::BAD_REQUEST,
        };
        Response::new(status, Bytes::new())
    }
}

impl std::fmt::Display for BodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => f.write_str("request body too large"),
            Self::Idle => f.write_str("request body idle"),
            Self::TooSlow => f.write_str("request body too slow"),
            Self::Closed(why) => write!(f, "request body failed: {why}"),
        }
    }
}

impl std::error::Error for BodyError {}

/// The lowest upload rate tolerated, averaged from the first byte asked for.
#[derive(Clone, Copy, Debug)]
pub struct MinRate {
    pub bytes_per_sec: u64,
    /// Rates are judged only once this much time has passed.
    pub window: Duration,
}

/// A request body read as it arrives, with the route's size cap, an idle
/// timeout and a minimum rate enforced while reading.
pub struct BodyStream {
    inner: Incoming,
    limit: usize,
    read: usize,
    idle: Duration,
    min_rate: MinRate,
    started: Instant,
}

impl BodyStream {
    pub(super) fn new(inner: Incoming, limit: usize, idle: Duration, min_rate: MinRate) -> Self {
        Self {
            inner,
            limit,
            read: 0,
            idle,
            min_rate,
            started: Instant::now(),
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Bytes read so far.
    pub fn read(&self) -> usize {
        self.read
    }

    /// The next chunk, `None` at the end of the body.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, BodyError> {
        use http_body_util::BodyExt;
        loop {
            let frame = tokio::time::timeout(self.idle, self.inner.frame())
                .await
                .map_err(|_| BodyError::Idle)?;
            let Some(frame) = frame else { return Ok(None) };
            let frame = frame.map_err(|err| BodyError::Closed(err.to_string()))?;
            let Ok(data) = frame.into_data() else {
                continue;
            };
            self.read = self.read.saturating_add(data.len());
            if self.read > self.limit {
                return Err(BodyError::TooLarge);
            }
            let elapsed = self.started.elapsed();
            if elapsed >= self.min_rate.window
                && (self.read as u128) * 1000
                    < self.min_rate.bytes_per_sec as u128 * elapsed.as_millis()
            {
                return Err(BodyError::TooSlow);
            }
            return Ok(Some(data));
        }
    }

    /// Reads the rest of the body into memory (still capped).
    pub async fn collect(mut self) -> Result<Bytes, BodyError> {
        let mut out = bytes::BytesMut::new();
        while let Some(chunk) = self.chunk().await? {
            out.extend_from_slice(&chunk);
        }
        Ok(out.freeze())
    }
}

/// A socket that errors once a write has made no progress for `timeout`,
/// so a peer that stops reading cannot hold a connection (and the task
/// feeding its body) forever.
pub(super) struct StallGuard<T> {
    inner: T,
    timeout: Duration,
    timer: Option<Pin<Box<Sleep>>>,
}

impl<T> StallGuard<T> {
    pub(super) fn new(inner: T, timeout: Duration) -> Self {
        Self {
            inner,
            timeout,
            timer: None,
        }
    }

    fn stalled<R>(
        &mut self,
        cx: &mut Context<'_>,
        poll: Poll<io::Result<R>>,
    ) -> Poll<io::Result<R>> {
        match poll {
            Poll::Ready(r) => {
                self.timer = None;
                Poll::Ready(r)
            }
            Poll::Pending => {
                let timeout = self.timeout;
                let timer = self
                    .timer
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)));
                match timer.as_mut().poll(cx) {
                    Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "peer stopped reading",
                    ))),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for StallGuard<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for StallGuard<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_write(cx, buf);
        this.stalled(cx, poll)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_flush(cx);
        this.stalled(cx, poll)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_write_vectored(cx, bufs);
        this.stalled(cx, poll)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}
