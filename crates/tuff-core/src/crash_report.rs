use std::backtrace::Backtrace;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, PoisonError, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

const WITTY: &[&str] = &[
    "Everything's going to plan. No, really, that was supposed to happen.",
    "Uh... Did I do that?",
    "Oops.",
    "Why did you do that?",
    "I feel sad now :(",
    "My bad.",
    "I'm sorry, Dave.",
    "I let you down. Sorry :(",
    "On the bright side, I bought you a teddy bear!",
    "Daisy, daisy...",
    "Oh - I know what I did wrong!",
    "Hey, that tickles! Hehehe!",
    "Don't be sad. I'll do better next time, I promise!",
    "Don't be sad, have a hug! <3",
    "I just don't know what went wrong :(",
    "Shall we play a game?",
    "Quite honestly, I wouldn't worry myself about that.",
    "I bet Cylons wouldn't have this problem.",
    "Sorry :(",
    "Surprise! Haha. Well, this is awkward.",
    "Would you like a cupcake?",
    "Hi. I'm tuff, and I'm a crashaholic.",
    "Ooh. Shiny.",
    "This doesn't make any sense!",
    "Why is it breaking :(",
    "Don't do that.",
    "Ouch. That hurt :(",
    "You're mean.",
    "There are four lights!",
    "But it works on my machine.",
];

static DIRECTORY: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn directory() -> &'static RwLock<Option<PathBuf>> {
    DIRECTORY.get_or_init(|| RwLock::new(None))
}

/// Makes `dir` the folder crash reports are written to.
pub fn set_directory(dir: PathBuf) {
    *directory().write().unwrap_or_else(PoisonError::into_inner) = Some(dir);
}

pub fn configured_directory() -> Option<PathBuf> {
    directory()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// A calendar date and time in UTC from Unix seconds.
fn civil(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The file stem for a report made at `secs`, like `crash-2026-10-17_12.30.05`.
pub fn file_stem(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil(secs);
    format!("crash-{y:04}-{mo:02}-{d:02}_{h:02}.{mi:02}.{s:02}")
}

fn system_report(out: &mut String) {
    let _ = writeln!(out, "-- System Details --");
    let _ = writeln!(out, "Details:");
    let _ = writeln!(out, "\tTuff Version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(
        out,
        "\tOperating System: {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let _ = writeln!(out, "\tProcessors: {cpus}");
    let _ = writeln!(out, "\tProcess ID: {}", std::process::id());
}

/// The text of a crash report: header, the failure, where it happened and a
/// system report.
pub fn render(
    secs: u64,
    description: &str,
    thread: &str,
    location: Option<(&str, u32)>,
    message: &str,
    backtrace: &Backtrace,
) -> String {
    let (y, mo, d, h, mi, s) = civil(secs);
    let witty = WITTY[(secs as usize) % WITTY.len()];
    let mut out = String::new();
    let _ = writeln!(out, "---- Tuff Crash Report ----");
    let _ = writeln!(out, "// {witty}");
    let _ = writeln!(out);
    let _ = writeln!(out, "Time: {y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC");
    let _ = writeln!(out, "Description: {description}");
    let _ = writeln!(out);
    let _ = writeln!(out, "{message}");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "A detailed walkthrough of the error, its code path and all known details is as follows:"
    );
    let _ = writeln!(out, "{}", "-".repeat(87));
    let _ = writeln!(out);
    let _ = writeln!(out, "-- Head --");
    let _ = writeln!(out, "Thread: {thread}");
    if let Some((file, line)) = location {
        let _ = writeln!(out, "Location: {file}:{line}");
    }
    let _ = writeln!(out, "Stacktrace:");
    let _ = writeln!(out, "{backtrace}");
    let _ = writeln!(out);
    system_report(&mut out);
    out
}

/// Writes a report into `dir` and returns its path.
pub fn save(dir: &Path, secs: u64, contents: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stem = file_stem(secs);
    let mut path = dir.join(format!("{stem}-server.txt"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}-server.txt"));
        n += 1;
    }
    std::fs::write(&path, contents)?;
    Ok(path)
}

/// Records a panic to the configured folder; does nothing when none is set.
pub fn record_panic(thread: &str, location: Option<(&str, u32)>, message: &str) -> Option<PathBuf> {
    let dir = configured_directory()?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let backtrace = Backtrace::force_capture();
    let text = render(
        secs,
        "Unexpected error",
        thread,
        location,
        message,
        &backtrace,
    );
    save(&dir, secs, &text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn civil_time_matches_known_instants() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(1_781_699_405), (2026, 6, 17, 12, 30, 5));
        assert_eq!(file_stem(951_782_400 + 59), "crash-2000-02-29_00.00.59");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_report_names_the_failure_and_the_system_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("tuff-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let text = render(
            100,
            "Unexpected error",
            "main",
            Some(("a.rs", 7)),
            "boom",
            &Backtrace::disabled(),
        );
        assert!(text.starts_with("---- Tuff Crash Report ----\n// "));
        assert!(text.contains("Description: Unexpected error"));
        assert!(text.contains("Location: a.rs:7"));
        assert!(text.contains("-- System Details --"));
        let first = save(&dir, 100, &text).unwrap();
        let second = save(&dir, 100, &text).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), text);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
