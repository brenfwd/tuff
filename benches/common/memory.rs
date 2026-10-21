//! Where a process's memory goes on macOS: `vmmap --summary` splits the
//! physical footprint from what malloc zones hold live, which RSS alone
//! cannot (an allocator keeps freed pages resident).

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

const MIB: f64 = 1024.0 * 1024.0;
const VMMAP_LIMIT: Duration = Duration::from_secs(120);

/// `vmmap --summary <pid>` parsed by [`parse_vmmap`]; `null` off macOS or
/// when vmmap fails, so a long run still writes its report.
pub async fn vmmap(pid: u32) -> Value {
    if !cfg!(target_os = "macos") {
        return Value::Null;
    }
    run_vmmap(pid).await.unwrap_or_else(|error| {
        eprintln!("[stress] vmmap {pid}: {error:#}");
        Value::Null
    })
}

async fn run_vmmap(pid: u32) -> Result<Value> {
    let run = tokio::process::Command::new("vmmap")
        .args(["--summary", &pid.to_string()])
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(VMMAP_LIMIT, run)
        .await
        .context("vmmap timed out")??;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_vmmap(&text).context("unrecognised vmmap output")
}

/// A vmmap size (`1376K`, `265.3M`, `1.6G`, or plain bytes) in MiB.
pub fn size_mib(text: &str) -> Option<f64> {
    let (number, scale) = match text.as_bytes().last()? {
        b'K' => (&text[..text.len() - 1], 1024.0),
        b'M' => (&text[..text.len() - 1], MIB),
        b'G' => (&text[..text.len() - 1], MIB * 1024.0),
        b'T' => (&text[..text.len() - 1], MIB * 1024.0 * 1024.0),
        _ => (text, 1.0),
    };
    Some(number.parse::<f64>().ok()? * scale / MIB)
}

/// The physical footprint, the region totals and the malloc zone totals
/// (resident, allocated and fragmentation) of a `vmmap --summary` report.
pub fn parse_vmmap(text: &str) -> Option<Value> {
    let field = |prefix: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .and_then(|rest| size_mib(rest.trim()))
    };
    let footprint = field("Physical footprint:")?;
    let peak = field("Physical footprint (peak):");
    let mut regions = None;
    let mut malloc = None;
    let mut in_malloc = false;
    for line in text.lines() {
        if line.starts_with("MALLOC ZONE") {
            in_malloc = true;
        }
        let Some(rest) = line
            .strip_prefix("TOTAL")
            .filter(|rest| rest.starts_with(char::is_whitespace))
        else {
            continue;
        };
        let columns: Vec<&str> = rest.split_whitespace().collect();
        if in_malloc {
            malloc = Some(json!({
                "resident_mib": size_mib(columns.get(1)?)?,
                "dirty_mib": size_mib(columns.get(2)?)?,
                "allocations": columns.get(4)?.parse::<u64>().ok()?,
                "allocated_mib": size_mib(columns.get(5)?)?,
                "frag_pct": columns.get(7)?.trim_end_matches('%').parse::<f64>().ok()?,
            }));
        } else {
            regions = Some(json!({
                "resident_mib": size_mib(columns.get(1)?)?,
                "dirty_mib": size_mib(columns.get(2)?)?,
                "swapped_mib": size_mib(columns.get(3)?)?,
            }));
        }
    }
    Some(json!({
        "footprint_mib": footprint,
        "footprint_peak_mib": peak,
        "regions": regions,
        "malloc": malloc,
        "dirtiest": dirtiest(text),
    }))
}

const DIRTIEST: usize = 10;

/// The region types holding the most dirty memory (`VM_ALLOCATE` is what
/// mimalloc maps, `Stack` thread stacks), from the table above the total.
fn dirtiest(text: &str) -> Value {
    let mut rows: Vec<(String, f64, f64)> = text
        .lines()
        .skip_while(|line| !line.starts_with("==========="))
        .skip(1)
        .take_while(|line| !line.starts_with("==========="))
        .filter_map(|line| {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let at = (1..tokens.len().saturating_sub(7)).find(|&i| {
                tokens[i..i + 7].iter().all(|t| size_mib(t).is_some())
                    && tokens[i + 7].parse::<u64>().is_ok()
            })?;
            let resident = size_mib(tokens[at + 1])?;
            let dirty = size_mib(tokens[at + 2])?;
            Some((tokens[..at].join(" "), resident, dirty))
        })
        .collect();
    rows.sort_by(|a, b| b.2.total_cmp(&a.2));
    rows.iter()
        .take(DIRTIEST)
        .map(|(name, resident, dirty)| json!({"region": name, "resident_mib": resident, "dirty_mib": dirty}))
        .collect()
}
