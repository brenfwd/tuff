// SPDX-License-Identifier: MIT
//! WebSocket, TCP and UDP client sockets as streams of events. Connecting
//! returns once the host reports the connection; `recv` then yields each
//! message or chunk in order, ending with a [`Close`] event.

use crate::bindings::tuff::plugin::capability_types as cap;
use crate::bindings::tuff::plugin::net;
use crate::exec::{Socket, SocketEvent};

pub use crate::exec::Close;
pub use cap::Denied as Error;

async fn open(handle: Result<u32, Error>) -> Result<Socket, Error> {
    let socket = Socket::new(handle?);
    loop {
        match socket.recv().await {
            Some(SocketEvent::Connected(outcome)) => return outcome.map(|()| socket),
            Some(SocketEvent::Closed(close)) => {
                return Err(close
                    .error
                    .unwrap_or_else(|| Error::Failed("closed while connecting".into())));
            }
            Some(_) => {}
            None => return Err(Error::Failed("socket gone".into())),
        }
    }
}

pub mod ws {
    use super::*;

    #[derive(Debug, Clone)]
    pub enum Event {
        Text(String),
        Binary(Vec<u8>),
        Closed(Close),
    }

    pub struct WebSocket {
        socket: Socket,
    }

    /// Connects to `url`, offering `protocols` as subprotocols.
    pub async fn connect(
        url: &str,
        headers: &[(&str, &str)],
        protocols: &[&str],
    ) -> Result<WebSocket, Error> {
        let headers: Vec<cap::Header> = headers
            .iter()
            .map(|(n, v)| cap::Header {
                name: (*n).to_owned(),
                value: v.as_bytes().to_vec(),
            })
            .collect();
        let protocols: Vec<String> = protocols.iter().map(|p| (*p).to_owned()).collect();
        let socket = open(net::ws_connect(url, &headers, &protocols)).await?;
        Ok(WebSocket { socket })
    }

    impl WebSocket {
        pub fn send_text(&self, text: &str) -> Result<(), Error> {
            net::ws_send(self.socket.handle(), &cap::WsMessage::Text(text.to_owned()))
        }

        pub fn send_binary(&self, data: &[u8]) -> Result<(), Error> {
            net::ws_send(self.socket.handle(), &cap::WsMessage::Binary(data.to_vec()))
        }

        /// The next message; None after the [`Event::Closed`] was taken.
        pub async fn recv(&self) -> Option<Event> {
            loop {
                match self.socket.recv().await? {
                    SocketEvent::Text(t) => return Some(Event::Text(t)),
                    SocketEvent::Data(d) => return Some(Event::Binary(d)),
                    SocketEvent::Closed(c) => return Some(Event::Closed(c)),
                    _ => {}
                }
            }
        }

        /// Closes the socket (also what dropping it does).
        pub fn close(self) {}
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Data(Vec<u8>),
    /// A UDP datagram and the address it came from.
    Datagram {
        peer: String,
        data: Vec<u8>,
    },
    Closed(Close),
}

async fn next(socket: &Socket) -> Option<Event> {
    loop {
        match socket.recv().await? {
            SocketEvent::Data(d) => return Some(Event::Data(d)),
            SocketEvent::Datagram { peer, data } => return Some(Event::Datagram { peer, data }),
            SocketEvent::Closed(c) => return Some(Event::Closed(c)),
            _ => {}
        }
    }
}

pub mod tcp {
    use super::*;

    pub use super::Event;

    pub struct Stream {
        socket: Socket,
    }

    pub async fn connect(host: &str, port: u16, tls: bool) -> Result<Stream, Error> {
        let socket = open(net::tcp_connect(host, port, tls)).await?;
        Ok(Stream { socket })
    }

    impl Stream {
        pub fn send(&self, data: &[u8]) -> Result<(), Error> {
            net::send(self.socket.handle(), data)
        }

        pub async fn recv(&self) -> Option<Event> {
            next(&self.socket).await
        }
    }
}

pub mod udp {
    use super::*;

    pub use super::Event;

    pub struct Socket {
        socket: crate::exec::Socket,
    }

    pub async fn connect(host: &str, port: u16) -> Result<Socket, Error> {
        let socket = open(net::udp_connect(host, port)).await?;
        Ok(Socket { socket })
    }

    impl Socket {
        pub fn send(&self, data: &[u8]) -> Result<(), Error> {
            net::send(self.socket.handle(), data)
        }

        pub async fn recv(&self) -> Option<Event> {
            next(&self.socket).await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::exec::{dispatch, spawn};

    fn connected(socket: u32) -> cap::IoEvent {
        cap::IoEvent::Connected(cap::Connected {
            socket,
            outcome: Ok(()),
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_socket_streams_events_in_order_then_ends() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let out = seen.clone();
        spawn(async move {
            let socket = open(Ok(501)).await.unwrap();
            while let Some(event) = next(&socket).await {
                out.borrow_mut().push(event);
            }
        });
        assert!(dispatch(connected(501)).is_none());
        for chunk in [b"a".to_vec(), b"bc".to_vec()] {
            let event = cap::IoEvent::Data(cap::SocketData {
                socket: 501,
                data: chunk,
            });
            assert!(dispatch(event).is_none());
        }
        let close = cap::IoEvent::Closed(cap::SocketClosed {
            socket: 501,
            error: None,
            code: Some(1000),
            reason: "bye".into(),
        });
        assert!(dispatch(close).is_none());
        let seen = seen.borrow();
        assert_eq!(seen.len(), 3);
        assert!(matches!(&seen[0], Event::Data(d) if d == b"a"));
        assert!(matches!(&seen[2], Event::Closed(c) if c.code == Some(1000)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_refused_connect_fails_and_unknown_sockets_pass_through() {
        let result = Rc::new(RefCell::new(None));
        let out = result.clone();
        spawn(async move {
            *out.borrow_mut() = Some(open(Ok(502)).await.map(|_| ()));
        });
        let refused = cap::IoEvent::Connected(cap::Connected {
            socket: 502,
            outcome: Err(Error::NotGranted("x".into())),
        });
        assert!(dispatch(refused).is_none());
        assert!(matches!(
            result.borrow().as_ref(),
            Some(Err(Error::NotGranted(_)))
        ));
        assert!(dispatch(connected(999)).is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn websocket_messages_become_text_and_binary_events() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let out = seen.clone();
        spawn(async move {
            let socket = open(Ok(503)).await.unwrap();
            while let Some(e) = socket.recv().await {
                out.borrow_mut().push(e);
            }
        });
        dispatch(connected(503));
        for message in [
            cap::WsMessage::Text("hi".into()),
            cap::WsMessage::Binary(vec![1]),
        ] {
            dispatch(cap::IoEvent::WsMessage(cap::WsFrame {
                socket: 503,
                message,
            }));
        }
        let seen = seen.borrow();
        assert!(matches!(&seen[0], SocketEvent::Text(t) if t == "hi"));
        assert!(matches!(&seen[1], SocketEvent::Data(d) if d == &[1]));
    }
}
