//! Ordered format migrations for every tuff-owned file format.
//!
//! Each format stamps a version and keeps a list of [`Step`]s, one per
//! version it can upgrade from, applied in order on load. Files newer than
//! the current version are refused; files older than the oldest step are
//! refused too. Adding a migration is adding one step and bumping the
//! format's current version.

use std::fmt;

/// Upgrades a value at version `from` to `from + 1`.
pub struct Step<T: ?Sized> {
    pub from: u32,
    pub apply: fn(&mut T) -> Result<(), String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Newer {
        format: &'static str,
        found: u32,
        current: u32,
    },
    Unsupported {
        format: &'static str,
        found: u32,
        current: u32,
    },
    Step {
        format: &'static str,
        from: u32,
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Newer {
                format,
                found,
                current,
            } => write!(
                f,
                "{format} version {found} is newer than this server supports ({current})"
            ),
            Self::Unsupported {
                format,
                found,
                current,
            } => write!(
                f,
                "{format} version {found} cannot be upgraded to version {current}"
            ),
            Self::Step {
                format,
                from,
                message,
            } => {
                write!(
                    f,
                    "upgrading {format} from version {from} failed: {message}"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

/// Brings `value`, stored at version `found`, to `current` by running
/// `steps` in order. Returns whether anything ran.
pub fn run<T: ?Sized>(
    format: &'static str,
    found: u32,
    current: u32,
    steps: &[Step<T>],
    value: &mut T,
) -> Result<bool, Error> {
    if found > current {
        return Err(Error::Newer {
            format,
            found,
            current,
        });
    }
    let mut version = found;
    while version < current {
        let step = steps
            .iter()
            .find(|step| step.from == version)
            .ok_or(Error::Unsupported {
                format,
                found,
                current,
            })?;
        (step.apply)(value).map_err(|message| Error::Step {
            format,
            from: version,
            message,
        })?;
        version += 1;
    }
    Ok(found != current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bump(value: &mut Vec<u32>) -> Result<(), String> {
        value.push(value.len() as u32);
        Ok(())
    }

    #[tuff_core::watchdog]
    #[test]
    fn current_runs_nothing_and_newer_or_unknown_is_refused() {
        let mut value = Vec::new();
        assert_eq!(run::<Vec<u32>>("f", 2, 2, &[], &mut value), Ok(false));
        assert!(matches!(
            run::<Vec<u32>>("f", 3, 2, &[], &mut value),
            Err(Error::Newer { .. })
        ));
        assert!(matches!(
            run::<Vec<u32>>("f", 1, 2, &[], &mut value),
            Err(Error::Unsupported { .. })
        ));
        assert!(value.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn steps_run_in_order_up_to_current() {
        let steps = [
            Step {
                from: 2,
                apply: bump,
            },
            Step {
                from: 1,
                apply: bump,
            },
        ];
        let mut value = Vec::new();
        assert_eq!(run("f", 1, 3, &steps, &mut value), Ok(true));
        assert_eq!(value, vec![0, 1]);
        let failing = [Step {
            from: 1,
            apply: |_: &mut Vec<u32>| Err("no".into()),
        }];
        assert!(matches!(
            run("f", 1, 2, &failing, &mut value),
            Err(Error::Step { from: 1, .. })
        ));
    }
}
