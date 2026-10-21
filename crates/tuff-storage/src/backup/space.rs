//! Free disk space, so uploads and restores that cannot fit are refused
//! before they start rather than failing half written.

use std::path::Path;

/// Bytes available to this process on the filesystem holding `path`, or
/// `None` where the platform cannot tell (then callers skip the check).
pub fn available(path: &Path) -> Option<u64> {
    imp::available(path)
}

/// Whether `needed` bytes fit in what is free at `path`; unknown space
/// counts as enough.
pub fn fits(path: &Path, needed: u64) -> bool {
    available(path).is_none_or(|free| free >= needed)
}

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub fn available(path: &Path) -> Option<u64> {
        let path = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stats = MaybeUninit::<libc::statvfs>::zeroed();
        // SAFETY: `path` is a NUL-terminated string that outlives the call
        // and `stats` is a `statvfs`-sized buffer `statvfs` fills when it
        // returns 0; it is read only then.
        let stats = unsafe {
            if libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) != 0 {
                return None;
            }
            stats.assume_init()
        };
        #[allow(clippy::unnecessary_cast)]
        let (blocks, size) = (stats.f_bavail as u64, stats.f_frsize as u64);
        Some(blocks.saturating_mul(size))
    }
}

#[cfg(not(unix))]
mod imp {
    use std::path::Path;

    pub fn available(_: &Path) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn free_space_is_known_on_unix_and_bounds_requests() {
        let dir = std::env::temp_dir();
        if cfg!(unix) {
            let free = available(&dir).expect("statvfs answers for the temp dir");
            assert!(free > 0);
            assert!(fits(&dir, 1));
            assert!(!fits(&dir, u64::MAX));
        }
        assert_eq!(available(Path::new("/no/such/place/at/all")), None);
        assert!(fits(Path::new("/no/such/place/at/all"), u64::MAX));
    }
}
