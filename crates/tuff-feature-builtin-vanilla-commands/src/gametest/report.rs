//! A JUnit-like XML report of a test run, as the headless runner's
//! `--report` writes it.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct JUnitReport {
    path: PathBuf,
    timestamp: String,
    started: Instant,
    cases: String,
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// `seconds` since the epoch as an ISO-8601 UTC instant.
fn iso_instant(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let secs = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

impl JUnitReport {
    pub fn new(path: PathBuf) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            path,
            timestamp: iso_instant(now),
            started: Instant::now(),
            cases: String::new(),
        }
    }

    fn case(&mut self, name: &str, structure: &str, millis: u64) {
        let _ = write!(
            self.cases,
            "<testcase classname=\"{}\" name=\"{}\" time=\"{}\"",
            escape(structure),
            escape(name),
            millis as f64 / 1000.0
        );
    }

    pub fn passed(&mut self, name: &str, structure: &str, millis: u64) {
        self.case(name, structure, millis);
        self.cases.push_str("/>");
    }

    /// A required test fails; an optional one counts as skipped.
    pub fn failed(
        &mut self,
        name: &str,
        structure: &str,
        millis: u64,
        required: bool,
        message: &str,
    ) {
        self.case(name, structure, millis);
        let _ = write!(
            self.cases,
            "><{} message=\"{}\"/></testcase>",
            if required { "failure" } else { "skipped" },
            escape(message)
        );
    }

    pub fn render(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?><testsuite><testsuite time=\"{}\" timestamp=\"{}\">{}</testsuite></testsuite>",
            self.started.elapsed().as_millis() as f64 / 1000.0,
            self.timestamp,
            self.cases
        )
    }

    pub fn finish(self) -> std::io::Result<()> {
        std::fs::write(&self.path, self.render())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn renders_passes_failures_and_skips() {
        let mut report = JUnitReport::new(PathBuf::from("unused.xml"));
        report.passed("minecraft:a", "minecraft:empty", 1500);
        report.failed("minecraft:b", "x:y", 0, true, "(1, 2, 3) bad <thing>");
        report.failed("minecraft:c", "x:y", 0, false, "meh");
        let xml = report.render();
        assert!(xml.contains(
            "<testcase classname=\"minecraft:empty\" name=\"minecraft:a\" time=\"1.5\"/>"
        ));
        assert!(xml.contains("<failure message=\"(1, 2, 3) bad &lt;thing&gt;\"/>"));
        assert!(xml.contains("<skipped message=\"meh\"/>"));
        assert!(xml.starts_with("<?xml"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn formats_utc_instants() {
        assert_eq!(iso_instant(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_instant(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
