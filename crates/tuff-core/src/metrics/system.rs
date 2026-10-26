//! Process-wide figures the actor samples itself: the tokio runtime, the
//! rayon pool, memory and CPU time.

use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RuntimeStats {
    pub workers: u64,
    pub alive_tasks: u64,
    pub global_queue_depth: u64,
    pub rayon_threads: u64,
}

impl RuntimeStats {
    /// The runtime the caller runs on; `None` outside one.
    pub fn sample() -> Option<Self> {
        let metrics = tokio::runtime::Handle::try_current().ok()?.metrics();
        Some(Self {
            workers: metrics.num_workers() as u64,
            alive_tasks: metrics.num_alive_tasks() as u64,
            global_queue_depth: metrics.global_queue_depth() as u64,
            rayon_threads: rayon::current_num_threads() as u64,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct MemoryStats {
    /// Resident set size.
    pub rss_bytes: u64,
    /// The largest resident set size so far, where known.
    pub peak_rss_bytes: Option<u64>,
}

impl MemoryStats {
    /// Linux reads `/proc/self/status`; macOS reads the kernel's task
    /// info. Other platforms have no way to ask without native
    /// dependencies, so they report nothing.
    pub fn sample() -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            parse_proc_status(&std::fs::read_to_string("/proc/self/status").ok()?)
        }
        #[cfg(target_os = "macos")]
        {
            sample_macos()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            None
        }
    }
}

#[cfg(target_os = "macos")]
fn sample_macos() -> Option<MemoryStats> {
    use std::mem::MaybeUninit;

    // SAFETY: `info` is a `mach_task_basic_info` sized buffer, zeroed
    // before the call. `task_info` with `MACH_TASK_BASIC_INFO` writes
    // exactly that struct into it when it returns KERN_SUCCESS; we check
    // the return code and the written count before reading the fields.
    // `mach_task_self` returns the calling task's own port, no ownership
    // to release.
    #[expect(
        deprecated,
        reason = "the replacement is unavailable on the supported platforms"
    )]
    unsafe {
        let mut info = MaybeUninit::<libc::mach_task_basic_info>::zeroed();
        let mut count = (std::mem::size_of::<libc::mach_task_basic_info>()
            / std::mem::size_of::<libc::integer_t>())
            as libc::mach_msg_type_number_t;
        let kr = libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr() as libc::task_info_t,
            &mut count,
        );
        if kr != libc::KERN_SUCCESS
            || count as usize * std::mem::size_of::<libc::integer_t>()
                < std::mem::size_of::<libc::mach_task_basic_info>()
        {
            return None;
        }
        let info = info.assume_init();
        Some(MemoryStats {
            rss_bytes: info.resident_size,
            peak_rss_bytes: Some(info.resident_size_max),
        })
    }
}

/// Shorter windows give too coarse a CPU figure to report.
pub const MIN_CPU_WINDOW: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct CpuStats {
    /// CPU time used per second of wall time, over the last window: 1.0
    /// is one core fully busy.
    pub cores: f64,
    /// Cores this process may run on, where known.
    pub host_cores: Option<u64>,
}

/// The process's user plus system CPU time at one instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuSample {
    pub at: Instant,
    pub cpu: Duration,
}

impl CpuSample {
    /// `None` where the platform offers no process CPU time.
    pub fn now() -> Option<Self> {
        Some(Self {
            at: Instant::now(),
            cpu: process_cpu_time()?,
        })
    }
}

/// Cores used between two samples, or `None` when the window is shorter
/// than [`MIN_CPU_WINDOW`] (the caller keeps the older sample).
pub fn cores_between(earlier: CpuSample, later: CpuSample) -> Option<f64> {
    let wall = later.at.checked_duration_since(earlier.at)?;
    if wall < MIN_CPU_WINDOW {
        return None;
    }
    let cpu = later.cpu.saturating_sub(earlier.cpu);
    Some(cpu.as_secs_f64() / wall.as_secs_f64())
}

pub fn host_cores() -> Option<u64> {
    std::thread::available_parallelism()
        .ok()
        .map(|n| n.get() as u64)
}

#[cfg(unix)]
fn process_cpu_time() -> Option<Duration> {
    use std::mem::MaybeUninit;

    let timeval = |tv: libc::timeval| {
        Duration::from_secs(u64::try_from(tv.tv_sec).ok()?)
            .checked_add(Duration::from_micros(u64::try_from(tv.tv_usec).ok()?))
    };
    // SAFETY: `getrusage` fills the whole `rusage` it is given when it
    // returns 0, which is checked before the value is read.
    let usage = unsafe {
        let mut usage = MaybeUninit::<libc::rusage>::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return None;
        }
        usage.assume_init()
    };
    timeval(usage.ru_utime)?.checked_add(timeval(usage.ru_stime)?)
}

#[cfg(not(unix))]
fn process_cpu_time() -> Option<Duration> {
    None
}

/// `VmRSS` and `VmHWM` from a `/proc/<pid>/status` text. These are in
/// KiB, unlike `statm`'s pages, whose size std cannot tell.
pub fn parse_proc_status(text: &str) -> Option<MemoryStats> {
    let field = |name: &str| -> Option<u64> {
        let line = text.lines().find(|line| line.starts_with(name))?;
        let rest = line[name.len()..].trim_start_matches(':');
        let kib: u64 = rest.split_whitespace().next()?.parse().ok()?;
        Some(kib * 1024)
    };
    Some(MemoryStats {
        rss_bytes: field("VmRSS")?,
        peak_rss_bytes: field("VmHWM"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tuff_core::watchdog]
    #[test]
    fn memory_sample_reports_resident_size() {
        let stats = MemoryStats::sample().expect("memory sample");
        assert!(stats.rss_bytes > 0);
        assert!(stats.peak_rss_bytes.unwrap_or(0) > 0);
    }

    fn at(base: Instant, ms: u64, cpu_ms: u64) -> CpuSample {
        CpuSample {
            at: base + Duration::from_millis(ms),
            cpu: Duration::from_millis(cpu_ms),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn cores_are_cpu_time_over_wall_time() {
        let base = Instant::now();
        assert_eq!(
            cores_between(at(base, 0, 0), at(base, 1000, 500)),
            Some(0.5)
        );
        assert_eq!(
            cores_between(at(base, 0, 1000), at(base, 2000, 5600)),
            Some(2.3)
        );
        assert_eq!(cores_between(at(base, 0, 0), at(base, 1000, 0)), Some(0.0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn short_reversed_or_shrinking_windows_report_no_cores() {
        let base = Instant::now();
        assert_eq!(cores_between(at(base, 0, 0), at(base, 99, 50)), None);
        assert_eq!(cores_between(at(base, 0, 0), at(base, 0, 0)), None);
        assert_eq!(cores_between(at(base, 500, 0), at(base, 0, 10)), None);
        assert_eq!(
            cores_between(at(base, 0, 900), at(base, 1000, 100)),
            Some(0.0)
        );
        assert_eq!(cores_between(at(base, 0, 0), at(base, 100, 100)), Some(1.0));
    }

    #[cfg(unix)]
    #[tuff_core::watchdog]
    #[test]
    fn process_cpu_time_grows_with_work() {
        let first = CpuSample::now().expect("cpu sample");
        let mut x = 0u64;
        let second = loop {
            for _ in 0..10_000 {
                x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(7));
            }
            let now = CpuSample::now().expect("cpu sample");
            if now.cpu >= first.cpu + Duration::from_millis(20)
                && now.at >= first.at + MIN_CPU_WINDOW
            {
                break now;
            }
        };
        let cores = cores_between(first, second).expect("long enough window");
        assert!(cores > 0.0, "{cores}");
        assert!(host_cores().unwrap_or(1) >= 1);
    }
}
