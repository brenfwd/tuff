//! The server's local calendar date, for the days the game marks
//! (Halloween). The local time zone comes from `TZ` or `/etc/localtime`
//! (TZif files and POSIX rule strings), read once; UTC when neither works.

use std::sync::OnceLock;

const MAX_ZONE_FILE: u64 = 256 * 1024;
const ZONEINFO_DIRS: &[&str] = &["/usr/share/zoneinfo", "/usr/lib/zoneinfo", "/etc/zoneinfo"];

/// A time zone: offsets from UTC in seconds by transition time, and the
/// rule for times after the last transition.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Zone {
    transitions: Vec<(i64, i32)>,
    initial: i32,
    rule: Option<Rule>,
}

/// A POSIX `TZ` rule: standard offset, and daylight saving time if any.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rule {
    std_offset: i32,
    dst: Option<Dst>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Dst {
    offset: i32,
    start: (Day, i32),
    end: (Day, i32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Day {
    /// Day 1..=365, February 29th never counted.
    Julian(u16),
    /// Day 0..=365, February 29th counted.
    Ordinal(u16),
    /// Weekday `d` (0 = Sunday) of week `w` (5 = last) of month `m`.
    NthWeekday(u8, u8, u8),
}

impl Zone {
    pub fn utc() -> Self {
        Self::default()
    }

    /// The offset from UTC, in seconds, at `unix` seconds.
    pub fn offset_at(&self, unix: i64) -> i32 {
        match self.transitions.iter().rposition(|(at, _)| *at <= unix) {
            None => match (self.transitions.is_empty(), self.rule) {
                (true, Some(rule)) => rule.offset_at(unix),
                _ => self.initial,
            },
            Some(last) if last + 1 == self.transitions.len() => match self.rule {
                Some(rule) => rule.offset_at(unix),
                None => self.transitions[last].1,
            },
            Some(index) => self.transitions[index].1,
        }
    }

    /// Reads a TZif file (any version); `None` if it is malformed.
    pub fn from_tzif(data: &[u8]) -> Option<Self> {
        let header = Header::read(data)?;
        if header.version == 0 {
            return header.zone(data.get(44..)?, 4);
        }
        let rest = data.get(44 + header.v1_len(4)?..)?;
        let header = Header::read(rest)?;
        let body = rest.get(44..)?;
        let mut zone = header.zone(body, 8)?;
        let footer = body.get(header.v1_len(8)?..)?;
        if let Some(text) = footer
            .strip_prefix(b"\n")
            .and_then(|f| f.split(|b| *b == b'\n').next())
            .and_then(|f| std::str::from_utf8(f).ok())
            .filter(|f| !f.is_empty())
        {
            zone.rule = Rule::parse(text);
        }
        Some(zone)
    }

    /// The zone a `TZ` value names: a rule string, or a zone file.
    fn from_tz(tz: &str) -> Option<Self> {
        let name = tz.strip_prefix(':').unwrap_or(tz);
        if name.is_empty() || name == "UTC" {
            return Some(Self::utc());
        }
        if !name.contains("..") {
            let paths = if name.starts_with('/') {
                vec![std::path::PathBuf::from(name)]
            } else {
                ZONEINFO_DIRS
                    .iter()
                    .map(|dir| std::path::Path::new(dir).join(name))
                    .collect()
            };
            if let Some(zone) = paths.iter().find_map(|path| read_zone_file(path)) {
                return Some(zone);
            }
        }
        Rule::parse(name).map(|rule| Self {
            rule: Some(rule),
            ..Self::default()
        })
    }
}

fn read_zone_file(path: &std::path::Path) -> Option<Zone> {
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_ZONE_FILE)
        .read_to_end(&mut data)
        .ok()?;
    Zone::from_tzif(&data)
}

struct Header {
    version: u8,
    is_ut: usize,
    is_std: usize,
    leap: usize,
    times: usize,
    types: usize,
    chars: usize,
}

impl Header {
    fn read(data: &[u8]) -> Option<Self> {
        if data.get(..4)? != b"TZif" {
            return None;
        }
        let version = match *data.get(4)? {
            0 => 0,
            b'2'..=b'9' => 2,
            _ => return None,
        };
        let count = |i: usize| -> Option<usize> {
            let bytes: [u8; 4] = data.get(20 + i * 4..24 + i * 4)?.try_into().ok()?;
            usize::try_from(u32::from_be_bytes(bytes)).ok()
        };
        Some(Self {
            version,
            is_ut: count(0)?,
            is_std: count(1)?,
            leap: count(2)?,
            times: count(3)?,
            types: count(4)?,
            chars: count(5)?,
        })
    }

    /// The data block's length with `width`-byte times.
    fn v1_len(&self, width: usize) -> Option<usize> {
        [
            self.times.checked_mul(width)?,
            self.times,
            self.types.checked_mul(6)?,
            self.chars,
            self.leap.checked_mul(width + 4)?,
            self.is_std,
            self.is_ut,
        ]
        .into_iter()
        .try_fold(0usize, usize::checked_add)
    }

    fn zone(&self, body: &[u8], width: usize) -> Option<Zone> {
        if self.types == 0 || body.len() < self.v1_len(width)? {
            return None;
        }
        let times = body.get(..self.times * width)?;
        let indices = body.get(self.times * width..self.times * (width + 1))?;
        let types =
            body.get(self.times * (width + 1)..self.times * (width + 1) + self.types * 6)?;
        let offset_of = |index: usize| -> Option<i32> {
            let bytes: [u8; 4] = types.get(index * 6..index * 6 + 4)?.try_into().ok()?;
            Some(i32::from_be_bytes(bytes))
        };
        let mut transitions = Vec::with_capacity(self.times);
        for (i, chunk) in times.chunks_exact(width).enumerate() {
            let at = if width == 8 {
                i64::from_be_bytes(chunk.try_into().ok()?)
            } else {
                i64::from(i32::from_be_bytes(chunk.try_into().ok()?))
            };
            let index = usize::from(*indices.get(i)?);
            transitions.push((at, offset_of(index)?));
        }
        Some(Zone {
            transitions,
            initial: offset_of(0)?,
            rule: None,
        })
    }
}

impl Rule {
    /// Parses a POSIX `TZ` rule such as `EST5EDT,M3.2.0,M11.1.0`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut p = Parser(text.as_bytes());
        p.name()?;
        let std_offset = -p.offset()?;
        if p.0.is_empty() {
            return Some(Self {
                std_offset,
                dst: None,
            });
        }
        p.name()?;
        let offset = if p.0.first().is_some_and(|c| *c != b',') {
            -p.offset()?
        } else {
            std_offset + 3600
        };
        let (start, end) = if p.0.is_empty() {
            (
                (Day::NthWeekday(3, 2, 0), 7200),
                (Day::NthWeekday(11, 1, 0), 7200),
            )
        } else {
            p.expect(b',')?;
            let start = p.transition()?;
            p.expect(b',')?;
            (start, p.transition()?)
        };
        if !p.0.is_empty() {
            return None;
        }
        Some(Self {
            std_offset,
            dst: Some(Dst { offset, start, end }),
        })
    }

    fn offset_at(&self, unix: i64) -> i32 {
        let Some(dst) = self.dst else {
            return self.std_offset;
        };
        let year = civil_from_days((unix + i64::from(self.std_offset)).div_euclid(86_400)).0;
        let start =
            dst.start.0.unix_midnight(year) + i64::from(dst.start.1) - i64::from(self.std_offset);
        let end = dst.end.0.unix_midnight(year) + i64::from(dst.end.1) - i64::from(dst.offset);
        let in_dst = if start <= end {
            unix >= start && unix < end
        } else {
            !(unix >= end && unix < start)
        };
        if in_dst { dst.offset } else { self.std_offset }
    }
}

impl Day {
    /// Midnight local time of this day in `year`, as seconds from the epoch
    /// counted without any offset.
    fn unix_midnight(self, year: i64) -> i64 {
        let jan1 = days_from_civil(year, 1, 1);
        let leap = is_leap(year);
        let day = match self {
            Self::Julian(n) => {
                let n = i64::from(n) - 1;
                jan1 + n + i64::from(leap && n >= 59)
            }
            Self::Ordinal(n) => jan1 + i64::from(n),
            Self::NthWeekday(m, w, d) => {
                let first = days_from_civil(year, u32::from(m), 1);
                let weekday = (first + 4).rem_euclid(7);
                let mut day =
                    first + (i64::from(d) - weekday).rem_euclid(7) + 7 * (i64::from(w) - 1);
                let next_month = if m == 12 {
                    days_from_civil(year + 1, 1, 1)
                } else {
                    days_from_civil(year, u32::from(m) + 1, 1)
                };
                while day >= next_month {
                    day -= 7;
                }
                day
            }
        };
        day * 86_400
    }
}

struct Parser<'a>(&'a [u8]);

impl Parser<'_> {
    fn name(&mut self) -> Option<()> {
        if self.0.first() == Some(&b'<') {
            let end = self.0.iter().position(|c| *c == b'>')?;
            self.0 = &self.0[end + 1..];
            return Some(());
        }
        let len = self
            .0
            .iter()
            .take_while(|c| c.is_ascii_alphabetic())
            .count();
        if len < 3 {
            return None;
        }
        self.0 = &self.0[len..];
        Some(())
    }

    fn expect(&mut self, byte: u8) -> Option<()> {
        (self.0.first() == Some(&byte)).then(|| self.0 = &self.0[1..])
    }

    fn number(&mut self, max: u32) -> Option<u32> {
        let len = self.0.iter().take_while(|c| c.is_ascii_digit()).count();
        if len == 0 || len > 3 {
            return None;
        }
        let value: u32 = std::str::from_utf8(&self.0[..len]).ok()?.parse().ok()?;
        self.0 = &self.0[len..];
        (value <= max).then_some(value)
    }

    /// `[+-]hh[:mm[:ss]]` in seconds.
    fn offset(&mut self) -> Option<i32> {
        let sign = match self.0.first() {
            Some(b'-') => {
                self.0 = &self.0[1..];
                -1
            }
            Some(b'+') => {
                self.0 = &self.0[1..];
                1
            }
            _ => 1,
        };
        let mut seconds = self.number(167)? as i32 * 3600;
        for unit in [60, 1] {
            if self.expect(b':').is_none() {
                break;
            }
            seconds += self.number(59)? as i32 * unit;
        }
        Some(sign * seconds)
    }

    fn transition(&mut self) -> Option<(Day, i32)> {
        let day = match self.0.first()? {
            b'J' => {
                self.0 = &self.0[1..];
                Day::Julian(self.number(365).filter(|n| *n >= 1)? as u16)
            }
            b'M' => {
                self.0 = &self.0[1..];
                let month = self.number(12).filter(|n| *n >= 1)? as u8;
                self.expect(b'.')?;
                let week = self.number(5).filter(|n| *n >= 1)? as u8;
                self.expect(b'.')?;
                Day::NthWeekday(month, week, self.number(6)? as u8)
            }
            _ => Day::Ordinal(self.number(365)? as u16),
        };
        let time = if self.expect(b'/').is_some() {
            self.offset()?
        } else {
            7200
        };
        Some((day, time))
    }
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The (year, month, day) of a day counted from 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The server's local zone, read once.
pub fn local_zone() -> &'static Zone {
    static ZONE: OnceLock<Zone> = OnceLock::new();
    ZONE.get_or_init(|| {
        match std::env::var("TZ") {
            Ok(tz) => Zone::from_tz(&tz),
            Err(_) => read_zone_file(std::path::Path::new("/etc/localtime")),
        }
        .unwrap_or_default()
    })
}

/// The local (month, day) at `unix` seconds in `zone`.
pub fn month_day(zone: &Zone, unix: i64) -> (u32, u32) {
    let local = unix + i64::from(zone.offset_at(unix));
    let (_, month, day) = civil_from_days(local.div_euclid(86_400));
    (month, day)
}

/// Whether it is October 31st in `zone` at `unix` seconds.
pub fn is_halloween(zone: &Zone, unix: i64) -> bool {
    month_day(zone, unix) == (10, 31)
}

/// Whether it is Halloween on the server's local calendar now.
pub fn halloween_now() -> bool {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .is_ok_and(|now| is_halloween(local_zone(), now.as_secs() as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OCT_31_2024_UTC: i64 = 1_730_332_800;

    fn tzif(version: u8, transitions: &[(i64, u8)], types: &[i32], footer: &str) -> Vec<u8> {
        let header = |width: usize| {
            let mut out = b"TZif".to_vec();
            out.push(version);
            out.extend([0; 15]);
            for count in [0, 0, 0, transitions.len(), types.len(), 4] {
                out.extend((count as u32).to_be_bytes());
            }
            for (at, _) in transitions {
                if width == 8 {
                    out.extend(at.to_be_bytes());
                } else {
                    out.extend((*at as i32).to_be_bytes());
                }
            }
            out.extend(transitions.iter().map(|(_, index)| *index));
            for offset in types {
                out.extend(offset.to_be_bytes());
                out.extend([0, 0]);
            }
            out.extend(b"ABC\0");
            out
        };
        let mut out = header(4);
        if version != 0 {
            out.extend(header(8));
            out.push(b'\n');
            out.extend(footer.as_bytes());
            out.push(b'\n');
        }
        out
    }

    #[tuff_core::watchdog]
    #[test]
    fn utc_halloween_is_october_31st() {
        let utc = Zone::utc();
        assert!(is_halloween(&utc, OCT_31_2024_UTC));
        assert!(is_halloween(&utc, OCT_31_2024_UTC + 86_399));
        assert!(!is_halloween(&utc, OCT_31_2024_UTC - 1));
        assert!(!is_halloween(&utc, OCT_31_2024_UTC + 86_400));
        assert!(is_halloween(&utc, OCT_31_2024_UTC + 365 * 86_400));
        assert!(!is_halloween(&utc, 0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn halloween_follows_the_local_date() {
        let new_york = Zone::from_tz("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(new_york.offset_at(OCT_31_2024_UTC), -4 * 3600);
        assert!(!is_halloween(&new_york, OCT_31_2024_UTC));
        assert!(is_halloween(&new_york, OCT_31_2024_UTC + 4 * 3600));
        assert!(is_halloween(&new_york, OCT_31_2024_UTC + 86_400 + 3 * 3600));
        let sydney = Zone::from_tz("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(sydney.offset_at(OCT_31_2024_UTC), 11 * 3600);
        assert!(is_halloween(&sydney, OCT_31_2024_UTC - 11 * 3600));
        assert!(!is_halloween(&sydney, OCT_31_2024_UTC + 13 * 3600));
    }

    #[tuff_core::watchdog]
    #[test]
    fn daylight_saving_switches_on_the_rule_days() {
        let new_york = Rule::parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
        let march_10_2024_7am_utc = 1_710_054_000;
        assert_eq!(new_york.offset_at(march_10_2024_7am_utc - 1), -5 * 3600);
        assert_eq!(new_york.offset_at(march_10_2024_7am_utc), -4 * 3600);
        let november_3_2024_6am_utc = 1_730_613_600;
        assert_eq!(new_york.offset_at(november_3_2024_6am_utc - 1), -4 * 3600);
        assert_eq!(new_york.offset_at(november_3_2024_6am_utc), -5 * 3600);
        assert_eq!(Rule::parse("<+0330>-3:30").unwrap().offset_at(0), 12_600);
        assert_eq!(Rule::parse("JST-9").unwrap().offset_at(0), 9 * 3600);
        assert!(Rule::parse("X1").is_none());
        assert!(Rule::parse("EST5EDT,M13.1.0,M11.1.0").is_none());
        assert!(Rule::parse("EST5EDT,M3.2.0").is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn zone_files_give_offsets_by_transition_then_footer() {
        let v1 = tzif(0, &[(1_000, 1)], &[3600, 7200], "");
        let zone = Zone::from_tzif(&v1).unwrap();
        assert_eq!(zone.offset_at(999), 3600);
        assert_eq!(zone.offset_at(1_000), 7200);
        let v2 = tzif(b'2', &[(1_000, 1)], &[0, -18_000], "EST5EDT,M3.2.0,M11.1.0");
        let zone = Zone::from_tzif(&v2).unwrap();
        assert_eq!(zone.offset_at(500), 0);
        assert_eq!(zone.offset_at(OCT_31_2024_UTC), -4 * 3600);
        assert_eq!(zone.offset_at(OCT_31_2024_UTC + 10 * 86_400), -5 * 3600);
        let slim = tzif(b'3', &[], &[0], "JST-9");
        assert_eq!(Zone::from_tzif(&slim).unwrap().offset_at(0), 9 * 3600);
    }

    #[tuff_core::watchdog]
    #[test]
    fn malformed_zone_files_are_rejected() {
        let good = tzif(b'2', &[(1_000, 1)], &[0, 3600], "CET-1");
        for len in 0..good.len() - 7 {
            let _ = Zone::from_tzif(&good[..len]);
        }
        assert!(Zone::from_tzif(&good[..40]).is_none());
        assert!(Zone::from_tzif(b"TZjf").is_none());
        let mut bad_index = tzif(0, &[(1_000, 9)], &[0], "");
        assert!(Zone::from_tzif(&bad_index).is_none());
        bad_index[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(Zone::from_tzif(&bad_index).is_none());
        let mut huge = tzif(0, &[], &[0], "");
        huge[32..36].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(Zone::from_tzif(&huge).is_none());
        assert_eq!(Zone::from_tz(":UTC"), Some(Zone::utc()));
        assert!(Zone::from_tz("../../etc/passwd").is_none());
    }
}
