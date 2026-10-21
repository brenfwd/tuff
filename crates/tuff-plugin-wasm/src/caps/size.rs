//! Byte sizes (`64 MiB`, `512KiB`, `4096`) and durations (`30s`,
//! `500ms`, `2m`) as owners write them.

use std::time::Duration;

pub fn bytes(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let scale: u64 = match unit.trim() {
        "" | "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        _ => {
            return Err(format!(
                "{text:?} is not a size (like 4096, 512 KiB or 64 MiB)"
            ));
        }
    };
    number_of(number, text)?
        .checked_mul(scale)
        .ok_or_else(|| format!("{text:?} is too large"))
}

pub fn duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number = number_of(number, text)?;
    let millis = match unit.trim() {
        "ms" => Some(number),
        "s" => number.checked_mul(1000),
        "m" => number.checked_mul(60_000),
        _ => {
            return Err(format!(
                "{text:?} is not a duration (like 500ms, 30s or 2m)"
            ));
        }
    };
    millis
        .map(Duration::from_millis)
        .ok_or_else(|| format!("{text:?} is too long"))
}

fn number_of(digits: &str, text: &str) -> Result<u64, String> {
    if digits.is_empty() || digits.len() > 19 {
        return Err(format!("{text:?} does not start with a number"));
    }
    digits
        .parse()
        .map_err(|_| format!("{text:?} does not start with a number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn sizes_and_durations_parse_with_units() {
        assert_eq!(bytes("4096"), Ok(4096));
        assert_eq!(bytes("64 MiB"), Ok(64 << 20));
        assert_eq!(bytes("512KiB"), Ok(512 << 10));
        assert_eq!(bytes("2 GiB"), Ok(2 << 30));
        for bad in [
            "",
            "MiB",
            "-1",
            "1.5 MiB",
            "10 MB",
            "99999999999999999999",
            "18446744073709551615 GiB",
        ] {
            assert!(bytes(bad).is_err(), "{bad}");
        }
        assert_eq!(duration("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(duration("2 m"), Ok(Duration::from_secs(120)));
        assert!(duration("5").is_err());
        assert!(duration("1h").is_err());
    }
}
