//! Process exit statuses a supervisor can tell apart, matching the table the
//! vanilla server exits with.

/// A crash during or after startup without a more specific cause.
pub const CRASH_DEFAULT: i32 = -1;
pub const CRASH_WITHOUT_REPORT: i32 = -2;
pub const CRASH_EARLY_NO_MODULES: i32 = -3;
pub const CRASH_EARLY_ARGUMENT_LIBRARY_LOAD: i32 = -4;
/// The command line could not be parsed.
pub const CRASH_EARLY_ARGUMENT_PARSE: i32 = -5;
pub const CRASH_SHUTDOWN: i32 = -6;
pub const VERSION_PARSING_FAIL: i32 = -7;
pub const CLIENT_WATCHDOG: i32 = -8;

/// Marks an error as a command line problem.
#[derive(Debug)]
pub struct ArgumentError;

impl std::fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid command line")
    }
}

/// The exit status a failed run ends with.
pub fn for_error(error: &anyhow::Error) -> i32 {
    if error.downcast_ref::<ArgumentError>().is_some() {
        CRASH_EARLY_ARGUMENT_PARSE
    } else {
        CRASH_DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[tuff_core::watchdog]
    #[test]
    fn argument_errors_exit_with_the_parse_status_and_others_with_the_default() {
        let parse: anyhow::Error = Err::<(), _>(anyhow::anyhow!("bad flag"))
            .context(ArgumentError)
            .unwrap_err();
        assert_eq!(for_error(&parse), -5);
        assert_eq!(for_error(&anyhow::anyhow!("disk full")), -1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_table_matches_vanilla() {
        let all = [
            CRASH_DEFAULT,
            CRASH_WITHOUT_REPORT,
            CRASH_EARLY_NO_MODULES,
            CRASH_EARLY_ARGUMENT_LIBRARY_LOAD,
            CRASH_EARLY_ARGUMENT_PARSE,
            CRASH_SHUTDOWN,
            VERSION_PARSING_FAIL,
            CLIENT_WATCHDOG,
        ];
        assert_eq!(all, [-1, -2, -3, -4, -5, -6, -7, -8]);
    }
}
