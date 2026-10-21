//! Numeric ranges as selector options and range arguments write them:
//! `5`, `..5`, `5..`, `3..7`.

use std::str::FromStr;

use super::error::CommandError;
use super::reader::Reader;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds<T> {
    pub min: Option<T>,
    pub max: Option<T>,
}

impl<T: Copy + PartialOrd> Bounds<T> {
    pub const ANY: Self = Self {
        min: None,
        max: None,
    };

    pub fn exactly(value: T) -> Self {
        Self {
            min: Some(value),
            max: Some(value),
        }
    }

    pub fn contains(&self, value: T) -> bool {
        self.min.is_none_or(|min| value >= min) && self.max.is_none_or(|max| value <= max)
    }
}

impl Bounds<f64> {
    /// Whether the square root of `value_sq` is in range, compared without
    /// taking roots.
    pub fn contains_sq(&self, value_sq: f64) -> bool {
        self.min.is_none_or(|min| value_sq >= min * min)
            && self.max.is_none_or(|max| value_sq <= max * max)
    }
}

fn is_number_char(c: char) -> bool {
    c.is_ascii_digit() || c == '-' || c == '.'
}

/// Reads a bound number: digits, `-` and `.`, but never the `..`
/// separator.
fn read_number<T: FromStr>(
    reader: &mut Reader<'_>,
    invalid: &str,
) -> Result<Option<T>, CommandError> {
    let start = reader.cursor();
    while reader.can_read()
        && reader.peek().is_some_and(is_number_char)
        && !(reader.peek_is('.') && reader.remaining().starts_with(".."))
    {
        reader.skip();
    }
    let number = &reader.input()[start..reader.cursor()];
    if number.is_empty() {
        return Ok(None);
    }
    number.parse().map(Some).map_err(|_| {
        reader.set_cursor(start);
        CommandError::dynamic(invalid, [number]).at(reader)
    })
}

fn parse<T: FromStr + Copy + PartialOrd>(
    reader: &mut Reader<'_>,
    invalid: &str,
) -> Result<Bounds<T>, CommandError> {
    let start = reader.cursor();
    if !reader.can_read() {
        return Err(CommandError::simple("argument.range.empty").at(reader));
    }
    let min: Option<T> = read_number(reader, invalid)?;
    let max = if reader.remaining().starts_with("..") {
        reader.skip();
        reader.skip();
        let max = read_number(reader, invalid)?;
        if min.is_none() && max.is_none() {
            reader.set_cursor(start);
            return Err(CommandError::simple("argument.range.empty").at(reader));
        }
        max
    } else {
        min
    };
    if min.is_none() && max.is_none() {
        reader.set_cursor(start);
        return Err(CommandError::simple("argument.range.empty").at(reader));
    }
    if let (Some(min), Some(max)) = (min, max)
        && min > max
    {
        reader.set_cursor(start);
        return Err(CommandError::simple("argument.range.swapped").at(reader));
    }
    Ok(Bounds { min, max })
}

pub fn parse_ints(reader: &mut Reader<'_>) -> Result<Bounds<i32>, CommandError> {
    parse(reader, "parsing.int.invalid")
}

pub fn parse_doubles(reader: &mut Reader<'_>) -> Result<Bounds<f64>, CommandError> {
    parse(reader, "parsing.double.invalid")
}

pub fn parse_floats(reader: &mut Reader<'_>) -> Result<Bounds<f32>, CommandError> {
    parse(reader, "parsing.float.invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ints(input: &str) -> Result<Bounds<i32>, String> {
        parse_ints(&mut Reader::new(input)).map_err(|e| e.key().unwrap().to_owned())
    }

    #[tuff_core::watchdog]
    #[test]
    fn parses_every_range_form() {
        assert_eq!(ints("5"), Ok(Bounds::exactly(5)));
        assert_eq!(
            ints("..5"),
            Ok(Bounds {
                min: None,
                max: Some(5)
            })
        );
        assert_eq!(
            ints("5.."),
            Ok(Bounds {
                min: Some(5),
                max: None
            })
        );
        assert_eq!(
            ints("3..7"),
            Ok(Bounds {
                min: Some(3),
                max: Some(7)
            })
        );
        assert_eq!(ints(""), Err("argument.range.empty".into()));
        assert_eq!(ints(".."), Err("argument.range.empty".into()));
        assert_eq!(ints("7..3"), Err("argument.range.swapped".into()));
        assert_eq!(ints("1.5"), Err("parsing.int.invalid".into()));
        let doubles = parse_doubles(&mut Reader::new("0.5..2.5")).unwrap();
        assert!(doubles.contains(1.0) && !doubles.contains(3.0));
        assert!(doubles.contains_sq(1.0) && !doubles.contains_sq(9.0));
    }
}
