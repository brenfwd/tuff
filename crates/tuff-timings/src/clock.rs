//! The span clock. Where the CPU has a constant-rate counter user code may
//! read (the ARM generic timer, x86's invariant TSC) a span reads it
//! directly: one instruction instead of a libc call and a timespec
//! conversion, which is about half of what an enabled span costs.
//! Elsewhere it counts nanoseconds of [`Instant`].

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Nanoseconds per tick in 32.32 fixed point; 0 until [`init`].
static SCALE: AtomicU64 = AtomicU64::new(0);
static SOURCE: OnceLock<Source> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The ARM virtual counter, `CNTVCT_EL0`.
    Cntvct,
    /// x86's invariant time stamp counter.
    Tsc,
    Monotonic,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Self::Cntvct => "cntvct",
            Self::Tsc => "tsc",
            Self::Monotonic => "monotonic",
        }
    }
}

/// Picks and calibrates the clock once per process.
pub fn init() -> Source {
    *SOURCE.get_or_init(|| {
        let (source, scale) = counter::detect().unwrap_or((Source::Monotonic, 1 << 32));
        SCALE.store(scale, Ordering::Relaxed);
        source
    })
}

/// The current tick count; only differences mean anything.
#[inline(always)]
pub fn now() -> u64 {
    counter::read()
}

/// `ticks` (a difference of [`now`]) in nanoseconds.
#[inline(always)]
pub fn ns(ticks: u64) -> u64 {
    ((u128::from(ticks) * u128::from(SCALE.load(Ordering::Relaxed)) + (1 << 31)) >> 32) as u64
}

#[cfg_attr(
    all(target_arch = "aarch64", any(target_os = "macos", target_os = "linux")),
    expect(dead_code, reason = "unused where the counter is read directly")
)]
fn monotonic() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

#[cfg(any(
    all(target_arch = "aarch64", any(target_os = "macos", target_os = "linux")),
    target_arch = "x86_64"
))]
fn scale(ns_per_second: f64, ticks_per_second: f64) -> Option<u64> {
    let scale = (ns_per_second / ticks_per_second * (1u64 << 32) as f64).round();
    (scale.is_finite() && scale >= 1.0 && scale < u64::MAX as f64).then_some(scale as u64)
}

#[cfg(all(target_arch = "aarch64", any(target_os = "macos", target_os = "linux")))]
mod counter {
    use super::Source;

    #[inline(always)]
    pub fn read() -> u64 {
        let ticks: u64;
        // SAFETY: macOS and Linux let user code read the virtual counter
        // (their own clocks do), and reading it has no side effects.
        unsafe {
            core::arch::asm!(
                "mrs {}, cntvct_el0",
                out(reg) ticks,
                options(nomem, nostack, preserves_flags)
            );
        }
        ticks
    }

    pub fn detect() -> Option<(Source, u64)> {
        let frequency: u64;
        // SAFETY: as for `read`; the frequency register is readable alongside.
        unsafe {
            core::arch::asm!(
                "mrs {}, cntfrq_el0",
                out(reg) frequency,
                options(nomem, nostack, preserves_flags)
            );
        }
        if frequency == 0 {
            return None;
        }
        Some((Source::Cntvct, super::scale(1e9, frequency as f64)?))
    }
}

#[cfg(target_arch = "x86_64")]
mod counter {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::Source;

    static TSC: AtomicBool = AtomicBool::new(false);

    #[inline(always)]
    pub fn read() -> u64 {
        if TSC.load(Ordering::Relaxed) {
            // SAFETY: every x86_64 CPU has RDTSC; it only reads a counter.
            unsafe { core::arch::x86_64::_rdtsc() }
        } else {
            super::monotonic()
        }
    }

    fn invariant_tsc() -> bool {
        use core::arch::x86_64::__cpuid;
        __cpuid(0x8000_0000).eax >= 0x8000_0007 && __cpuid(0x8000_0007).edx & (1 << 8) != 0
    }

    /// One (TSC, Instant) pair, retried until the two reads are close.
    fn pair() -> (u64, Instant) {
        let mut best = None;
        for _ in 0..16 {
            // SAFETY: as in `read`.
            let (before, at, after) = unsafe {
                let before = core::arch::x86_64::_rdtsc();
                let at = Instant::now();
                (before, at, core::arch::x86_64::_rdtsc())
            };
            let spread = after.wrapping_sub(before);
            if best.is_none_or(|(s, _, _)| spread < s) {
                best = Some((spread, before + spread / 2, at));
            }
        }
        let (_, tsc, at) = best.expect("sampled");
        (tsc, at)
    }

    pub fn detect() -> Option<(Source, u64)> {
        if !invariant_tsc() {
            return None;
        }
        let (tsc0, at0) = pair();
        while at0.elapsed() < Duration::from_millis(5) {
            std::hint::spin_loop();
        }
        let (tsc1, at1) = pair();
        let ticks = tsc1.checked_sub(tsc0)? as f64;
        let ns = at1.duration_since(at0).as_nanos() as f64;
        let scale = super::scale(ns, ticks)?;
        TSC.store(true, Ordering::Relaxed);
        Some((Source::Tsc, scale))
    }
}

#[cfg(not(any(
    all(target_arch = "aarch64", any(target_os = "macos", target_os = "linux")),
    target_arch = "x86_64"
)))]
mod counter {
    use super::Source;

    #[inline(always)]
    pub fn read() -> u64 {
        super::monotonic()
    }

    pub fn detect() -> Option<(Source, u64)> {
        None
    }
}

/// How far the clock drifts from [`Instant`] over `over`, as a ratio
/// (1.0 is exact); for tests.
#[doc(hidden)]
pub fn drift(over: Duration) -> f64 {
    init();
    let (ticks0, at0) = (now(), Instant::now());
    while at0.elapsed() < over {
        std::hint::spin_loop();
    }
    let (ticks1, at1) = (now(), Instant::now());
    ns(ticks1.wrapping_sub(ticks0)) as f64 / at1.duration_since(at0).as_nanos() as f64
}
