//! Lets a human join a bench server with a real client and watch:
//! `--ops <a,b,..>` makes those players operators (level 4), `--host <ip>`
//! (127.0.0.1) and `--port <n>` (0, an ephemeral port) choose the address.
//! Bench servers run in offline mode, so ops are keyed by offline UUIDs,
//! which depend on a name's case; names match players ignoring case.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use anyhow::{Context, Result, bail};
use tuff::ServerHandle;

const MAX_NAME: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    pub ops: Vec<String>,
    pub host: IpAddr,
    pub port: u16,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            ops: Vec::new(),
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
        }
    }
}

impl Watch {
    /// Takes `flag`'s value when it is one of the watch flags, returning
    /// whether it was.
    pub fn flag(&mut self, flag: &str, value: impl FnOnce() -> Result<String>) -> Result<bool> {
        match flag {
            "--ops" => self.ops = names(&value()?)?,
            "--host" => {
                self.host = value()?
                    .parse()
                    .context("--host takes an IPv4 or IPv6 address")?
            }
            "--port" => self.port = value()?.parse().context("--port takes a port number")?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub fn bind(&self) -> String {
        SocketAddr::new(self.host, self.port).to_string()
    }

    /// Fails if a watcher would share a name with a bot.
    pub fn check_bots<'a>(&self, bots: impl IntoIterator<Item = &'a str>) -> Result<()> {
        for bot in bots {
            if let Some(op) = self.ops.iter().find(|op| op.eq_ignore_ascii_case(bot)) {
                bail!("--ops name {op} is taken by a bench bot");
            }
        }
        Ok(())
    }

    /// The line telling a human where to connect.
    pub fn join_hint(&self, addr: SocketAddr) -> String {
        let ops = if self.ops.is_empty() {
            String::new()
        } else {
            format!(" (operators: {})", self.ops.join(", "))
        };
        if addr.ip().is_unspecified() {
            format!(
                "join with a client at this machine's address, port {}{ops}",
                addr.port()
            )
        } else {
            format!("join with a client at {addr}{ops}")
        }
    }
}

/// `addr` with an unspecified IP replaced by loopback, for local bots.
pub fn reachable(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), addr.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), addr.port())
        }
        _ => addr,
    }
}

/// A comma-separated list of player names, deduplicated ignoring case.
pub fn names(text: &str) -> Result<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    for name in text
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if name.len() > MAX_NAME || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!("--ops: {name:?} is not a valid player name");
        }
        if !names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
            names.push(name.to_owned());
        }
    }
    if names.is_empty() {
        bail!("--ops needs at least one player name");
    }
    Ok(names)
}

/// Makes each named player an operator at the server's op level, under
/// the exact case given. Offline UUIDs depend on that case, so
/// [`keep_ops`] also ops players who join with another case.
pub async fn apply_ops(server: &ServerHandle, names: &[String]) -> Result<()> {
    for name in names {
        op(server, name).await?;
    }
    Ok(())
}

async fn op(server: &ServerHandle, name: &str) -> Result<()> {
    let lines = server.run_command(format!("op {name}")).await?;
    let opped = lines.iter().any(|line| {
        let json = line.to_json();
        json.contains("commands.op.success") || json.contains("commands.op.failed")
    });
    if !opped {
        let answer: Vec<String> = lines.iter().map(|line| line.plain()).collect();
        bail!("could not op {name}: {}", answer.join("; "));
    }
    Ok(())
}

/// Ops each online player whose name matches one of `names` ignoring
/// case and is not in `opped` yet, under the case they joined with.
/// Returns the players it opped and adds them to `opped`.
pub async fn op_joined(
    server: &ServerHandle,
    names: &[String],
    opped: &mut Vec<String>,
) -> Result<Vec<String>> {
    let mut fresh = Vec::new();
    for player in online(server, names).await? {
        if !opped.contains(&player) {
            op(server, &player).await?;
            opped.push(player.clone());
            fresh.push(player);
        }
    }
    Ok(fresh)
}

/// Keeps opping players named by `names` (ignoring case) as they join,
/// until the server stops or the task is aborted.
pub fn keep_ops(server: ServerHandle, names: Vec<String>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if names.is_empty() {
            return;
        }
        let mut opped = Vec::new();
        let mut every = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            every.tick().await;
            if op_joined(&server, &names, &mut opped).await.is_err() {
                return;
            }
        }
    })
}

/// Every online player's name, as they joined.
pub async fn players(server: &ServerHandle) -> Result<Vec<String>> {
    let lines = server.run_command("list").await?;
    let list = lines
        .iter()
        .find(|line| line.translate.as_deref() == Some("commands.list.players"))
        .and_then(|line| line.with.get(2))
        .context("list gave no player list")?
        .plain();
    Ok(list
        .split(", ")
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The online players whose names match one of `names` ignoring case, as
/// they joined.
pub async fn online(server: &ServerHandle, names: &[String]) -> Result<Vec<String>> {
    Ok(players(server)
        .await?
        .into_iter()
        .filter(|player| names.iter().any(|name| name.eq_ignore_ascii_case(player)))
        .collect())
}
