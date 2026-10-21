//! What the server holds once the window ends and after every bot has
//! left: RSS over `--linger`, the bench's live heap (tuff runs in this
//! process), loaded chunks and regions, and a vmmap summary.

use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};

use crate::alloc;
use crate::memory;
use crate::servers::{BenchServer, Kind};

const MIB: f64 = 1024.0 * 1024.0;
const EVERY: Duration = Duration::from_secs(1);

fn live_heap(server: &BenchServer) -> Value {
    match server {
        BenchServer::Tuff { .. } => (alloc::live_bytes() as f64 / MIB).into(),
        BenchServer::Vanilla(_) => Value::Null,
    }
}

async fn rss_mib(pid: u32) -> Option<f64> {
    tokio::task::spawn_blocking(move || tuff_bot::report::sample_process(pid))
        .await
        .ok()
        .flatten()
        .map(|sample| sample.rss_bytes as f64 / MIB)
}

/// Loaded chunks and regions as tuff's metrics see them now; `null` for
/// vanilla.
async fn loaded(server: &BenchServer) -> Result<Value> {
    let BenchServer::Tuff { handle, .. } = server else {
        return Ok(Value::Null);
    };
    let snapshot = handle.metrics().await?.collect().await;
    let manager = snapshot.manager.as_ref();
    Ok(json!({
        "reported_chunks": snapshot.world.chunks,
        "reported_regions": snapshot.world.regions,
        "regions": manager.map(|m| m.regions),
        "closing_regions": manager.map(|m| m.closing),
        "held_chunks": manager.map(|m| m.held_chunks),
        "open_region_files": snapshot.chunk_io.as_ref().map(|io| io.open_files),
    }))
}

/// RSS, live heap, what is loaded and a vmmap summary before any player
/// joins.
pub async fn at_start(server: &BenchServer) -> Result<Value> {
    let pid = server.pid();
    Ok(json!({
        "rss_mib": rss_mib(pid).await,
        "live_heap_mib": live_heap(server),
        "loaded": loaded(server).await?,
        "vmmap": memory::vmmap(pid).await,
    }))
}

/// Live heap and a vmmap summary at the end of the window.
pub async fn at_end(server: &BenchServer) -> Result<Value> {
    Ok(json!({
        "live_heap_mib": live_heap(server),
        "vmmap": memory::vmmap(server.pid()).await,
    }))
}

/// Samples RSS every second for `linger` after the last bot left, then
/// reports what is still loaded and resident.
pub async fn after_leave(server: &BenchServer, kind: Kind, linger: Duration) -> Result<Value> {
    let pid = server.pid();
    let start = Instant::now();
    let mut every = tokio::time::interval(EVERY);
    let mut rss = Vec::new();
    loop {
        every.tick().await;
        if let Some(mib) = rss_mib(pid).await {
            rss.push(json!([start.elapsed().as_secs_f64().round(), mib]));
        }
        if start.elapsed() >= linger {
            break;
        }
    }
    let at = |index: usize| rss.get(index).map(|pair| pair[1].clone());
    let min = rss
        .iter()
        .filter_map(|pair| pair[1].as_f64())
        .fold(f64::INFINITY, f64::min);
    Ok(json!({
        "server": kind.name(),
        "linger_s": linger.as_secs_f64(),
        "rss_at_leave_mib": at(0),
        "rss_end_mib": rss.last().map(|pair| pair[1].clone()),
        "rss_min_mib": min.is_finite().then_some(min),
        "live_heap_mib": live_heap(server),
        "loaded": loaded(server).await?,
        "vmmap": memory::vmmap(pid).await,
        "rss_mib": rss,
    }))
}

/// `after_leave` against `at_start`: each figure's ratio, so a run shows
/// whether memory came back to where the server started.
pub fn idle_vs_start(start: &Value, after: &Value) -> Value {
    let ratio = |pointer: &str, after_pointer: &str| {
        let start = start.pointer(pointer)?.as_f64()?;
        let after = after.pointer(after_pointer)?.as_f64()?;
        (start > 0.0).then(|| after / start)
    };
    json!({
        "footprint_ratio": ratio("/vmmap/footprint_mib", "/vmmap/footprint_mib"),
        "dirty_ratio": ratio("/vmmap/regions/dirty_mib", "/vmmap/regions/dirty_mib"),
        "live_heap_ratio": ratio("/live_heap_mib", "/live_heap_mib"),
        "rss_ratio": ratio("/rss_mib", "/rss_end_mib"),
    })
}
