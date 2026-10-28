//! A TCP proxy that delays each direction by half the configured round trip,
//! so recorded real-client sessions see realistic network latency.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::Instant;

const CHUNK: usize = 16 * 1024;

pub struct LatencyProxy {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl LatencyProxy {
    /// Listens on the loopback of the same family as `target` and forwards to
    /// it with `rtt / 2` of delay each way. A zero `rtt` still proxies.
    pub async fn start(target: SocketAddr, rtt: Duration) -> std::io::Result<Self> {
        let bind: SocketAddr = if target.is_ipv6() {
            "[::1]:0".parse().unwrap()
        } else {
            "127.0.0.1:0".parse().unwrap()
        };
        let listener = TcpListener::bind(bind).await?;
        let addr = listener.local_addr()?;
        let one_way = rtt / 2;
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                tokio::spawn(async move {
                    if let Ok(server) = TcpStream::connect(target).await {
                        let _ = client.set_nodelay(true);
                        let _ = server.set_nodelay(true);
                        pipe(client, server, one_way).await;
                    }
                });
            }
        });
        Ok(Self { addr, task })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for LatencyProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn pipe(client: TcpStream, server: TcpStream, one_way: Duration) {
    let (cr, cw) = client.into_split();
    let (sr, sw) = server.into_split();
    let up = tokio::spawn(delayed_copy(cr, sw, one_way));
    let down = tokio::spawn(delayed_copy(sr, cw, one_way));
    let _ = tokio::join!(up, down);
}

async fn delayed_copy(
    mut from: impl AsyncReadExt + Unpin,
    mut to: impl AsyncWriteExt + Unpin + Send + 'static,
    one_way: Duration,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
    let writer = tokio::spawn(async move {
        while let Some((due, bytes)) = rx.recv().await {
            tokio::time::sleep_until(due).await;
            if to.write_all(&bytes).await.is_err() {
                return;
            }
        }
        let _ = to.shutdown().await;
    });
    let mut buf = vec![0u8; CHUNK];
    while let Ok(n) = from.read(&mut buf).await {
        if n == 0
            || tx
                .send((Instant::now() + one_way, buf[..n].to_vec()))
                .is_err()
        {
            break;
        }
    }
    drop(tx);
    let _ = writer.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn echo_server() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        addr
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn round_trip_takes_the_configured_rtt() {
        let target = echo_server().await;
        let proxy = LatencyProxy::start(target, Duration::from_millis(100))
            .await
            .unwrap();
        let mut c = TcpStream::connect(proxy.addr()).await.unwrap();
        let start = Instant::now();
        c.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        c.read_exact(&mut got).await.unwrap();
        let took = start.elapsed();
        assert_eq!(&got, b"ping");
        assert!(took >= Duration::from_millis(95), "{took:?}");
        assert!(took < Duration::from_millis(400), "{took:?}");
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn one_mebibyte_arrives_intact_and_in_order() {
        let target = echo_server().await;
        let proxy = LatencyProxy::start(target, Duration::from_millis(20))
            .await
            .unwrap();
        let payload: Vec<u8> = (0..(1usize << 20)).map(|i| (i * 31 % 251) as u8).collect();
        let (mut r, mut w) = TcpStream::connect(proxy.addr()).await.unwrap().into_split();
        let send = payload.clone();
        let writer = tokio::spawn(async move {
            w.write_all(&send).await.unwrap();
            w
        });
        let mut got = vec![0u8; payload.len()];
        r.read_exact(&mut got).await.unwrap();
        let _w = writer.await.unwrap();
        assert!(got == payload);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn zero_rtt_still_forwards() {
        let target = echo_server().await;
        let proxy = LatencyProxy::start(target, Duration::ZERO).await.unwrap();
        let mut c = TcpStream::connect(proxy.addr()).await.unwrap();
        c.write_all(b"hi").await.unwrap();
        let mut got = [0u8; 2];
        c.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hi");
    }
}
