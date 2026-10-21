use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use crate::protocol::{Command, parse_response};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const ACCEPT_TIMEOUT: Duration = Duration::from_secs(180);

pub type CallResult = Result<String, String>;

pub struct Call {
    pub command: Command,
    pub reply: oneshot::Sender<CallResult>,
}

/// A loopback listener on an ephemeral port plus the shared secret the hook
/// must present; passed to the client as system properties.
pub struct ControlListener {
    listener: TcpListener,
    pub port: u16,
    pub token: String,
}

impl ControlListener {
    pub async fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            listener,
            port,
            token: make_token(),
        })
    }

    pub fn jvm_args(&self) -> Vec<String> {
        vec![
            format!("-Dtuff.harness.port={}", self.port),
            format!("-Dtuff.harness.token={}", self.token),
        ]
    }

    /// Starts the control actor; it accepts the hook's connection, then
    /// serves calls until the connection closes or every sender is dropped.
    pub fn serve(self) -> mpsc::Sender<Call> {
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(run(self, rx));
        tx
    }
}

fn make_token() -> String {
    use sha1::{Digest, Sha1};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha1::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(format!("{:p}", &hasher as *const _));
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn accept(listener: ControlListener) -> Result<BufReader<TcpStream>> {
    let (stream, _) = timeout(ACCEPT_TIMEOUT, listener.listener.accept())
        .await
        .context("client hook never connected")??;
    stream.set_nodelay(true)?;
    let mut reader = BufReader::new(stream);
    let mut hello = String::new();
    timeout(Duration::from_secs(10), reader.read_line(&mut hello))
        .await
        .context("no hello from the hook")??;
    if hello.trim_end() != format!("0 hello {}", listener.token) {
        bail!("hook presented a bad token");
    }
    Ok(reader)
}

async fn run(listener: ControlListener, mut calls: mpsc::Receiver<Call>) {
    let mut reader = match accept(listener).await {
        Ok(reader) => reader,
        Err(e) => {
            log::warn!("client control: {e:#}");
            return;
        }
    };
    let mut pending: HashMap<u64, oneshot::Sender<CallResult>> = HashMap::new();
    let mut next_id = 1u64;
    let mut line = String::new();
    loop {
        tokio::select! {
            call = calls.recv() => {
                let Some(call) = call else { break };
                if call.reply.is_closed() {
                    continue;
                }
                let id = next_id;
                next_id += 1;
                let wire = format!("{}\n", call.command.encode(id));
                if reader.get_mut().write_all(wire.as_bytes()).await.is_err() {
                    let _ = call.reply.send(Err(anyhow!("control connection closed").to_string()));
                    break;
                }
                pending.insert(id, call.reply);
            }
            read = reader.read_line(&mut line) => {
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                match parse_response(line.trim_end()) {
                    Ok(response) => {
                        if let Some(tx) = pending.remove(&response.id) {
                            let _ = tx.send(response.outcome);
                        }
                    }
                    Err(e) => log::warn!("client control: {e:#}"),
                }
                line.clear();
            }
        }
    }
    for (_, tx) in pending {
        let _ = tx.send(Err("control connection closed".into()));
    }
}
