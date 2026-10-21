//! Server-sent events: `text/event-stream` framing with keep-alive pings.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hyper::StatusCode;
use tokio::sync::mpsc;

use super::{Body, Response};

/// How often an idle event stream sends a comment so proxies and the
/// response idle timeout keep it open.
pub const PING_INTERVAL: Duration = Duration::from_secs(15);
const OUT_QUEUE: usize = 16;

/// One event. `id` and `event` with a line break are dropped rather than
/// split, so a value can never inject a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub id: Option<String>,
    pub event: Option<String>,
    pub data: Arc<str>,
}

impl Event {
    pub fn new(event: impl Into<String>, data: impl Into<Arc<str>>) -> Self {
        Self {
            id: None,
            event: Some(event.into()),
            data: data.into(),
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// The event's wire form, ending in the blank line that dispatches it.
    pub fn frame(&self) -> String {
        let mut out = String::with_capacity(self.data.len() + 32);
        let single = |v: &&String| !v.contains(['\r', '\n', '\0']);
        if let Some(id) = self.id.as_ref().filter(single) {
            out.push_str("id: ");
            out.push_str(id);
            out.push('\n');
        }
        if let Some(event) = self.event.as_ref().filter(single) {
            out.push_str("event: ");
            out.push_str(event);
            out.push('\n');
        }
        let data = self.data.replace("\r\n", "\n").replace('\r', "\n");
        for line in data.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        out
    }
}

impl Response {
    /// A `text/event-stream` response sending each event from `events`,
    /// with a ping comment after every [`PING_INTERVAL`] of quiet. It ends
    /// when `events` closes; a gone client closes `events`' receiver.
    pub fn event_stream(events: mpsc::Receiver<Event>) -> Response {
        Self::event_stream_with(events, PING_INTERVAL)
    }

    pub fn event_stream_with(mut events: mpsc::Receiver<Event>, ping: Duration) -> Response {
        let (tx, rx) = mpsc::channel(OUT_QUEUE);
        tokio::spawn(async move {
            let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + ping, ping);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let frame = tokio::select! {
                    event = events.recv() => match event {
                        Some(event) => {
                            timer.reset();
                            Bytes::from(event.frame())
                        }
                        None => return,
                    },
                    _ = timer.tick() => Bytes::from_static(b": ping\n\n"),
                    _ = tx.closed() => return,
                };
                if tx.send(frame).await.is_err() {
                    return;
                }
            }
        });
        let mut resp = Response::new(StatusCode::OK, Bytes::new())
            .header("content-type", "text/event-stream; charset=utf-8")
            .header("cache-control", "no-store")
            .header("x-accel-buffering", "no");
        resp.body = Body::Stream(rx);
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn frames_split_lines_and_drop_injected_fields() {
        let event = Event::new("logs", "a\nb\r\nc").with_id("7");
        assert_eq!(
            event.frame(),
            "id: 7\nevent: logs\ndata: a\ndata: b\ndata: c\n\n"
        );
        let hostile = Event {
            id: Some("1\ndata: x".into()),
            event: Some("e\r".into()),
            data: Arc::from(""),
        };
        assert_eq!(hostile.frame(), "data: \n\n");
    }
}
