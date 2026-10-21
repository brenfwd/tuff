//! A cursor over command text with the game's reading rules: which
//! characters make up numbers and bare words, how quoting works, and which
//! translation key each failure reports.

use super::error::CommandError;

/// Characters the game allows in an unquoted string.
pub fn is_unquoted_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+')
}

fn is_number_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '.' | '-')
}

fn is_quote(c: char) -> bool {
    matches!(c, '"' | '\'')
}

#[derive(Debug, Clone)]
pub struct Reader<'a> {
    input: &'a str,
    cursor: usize,
}

impl<'a> Reader<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input, cursor: 0 }
    }

    pub fn input(&self) -> &'a str {
        self.input
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor;
    }

    pub fn remaining(&self) -> &'a str {
        &self.input[self.cursor..]
    }

    pub fn remaining_len(&self) -> usize {
        self.input.len() - self.cursor
    }

    pub fn can_read(&self) -> bool {
        self.cursor < self.input.len()
    }

    pub fn can_read_n(&self, n: usize) -> bool {
        self.cursor + n <= self.input.len()
    }

    pub fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    pub fn peek_is(&self, c: char) -> bool {
        self.peek() == Some(c)
    }

    pub fn read(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.cursor += c.len_utf8();
        Some(c)
    }

    pub fn skip(&mut self) {
        self.read();
    }

    pub fn skip_whitespace(&mut self) {
        let rest = self.remaining();
        self.cursor += rest.len() - rest.trim_start().len();
    }

    /// Consumes and returns the run of characters satisfying `allowed`.
    pub fn read_while(&mut self, allowed: impl Fn(char) -> bool) -> &'a str {
        let rest = self.remaining();
        let len = rest
            .char_indices()
            .find(|(_, c)| !allowed(*c))
            .map_or(rest.len(), |(i, _)| i);
        self.cursor += len;
        &rest[..len]
    }

    /// The next word up to a space, without consuming it.
    pub fn peek_word(&self) -> &'a str {
        let rest = self.remaining();
        &rest[..rest.find(' ').unwrap_or(rest.len())]
    }

    pub fn read_int(&mut self) -> Result<i32, CommandError> {
        let start = self.cursor;
        let number = self.read_while(is_number_char);
        if number.is_empty() {
            return Err(CommandError::simple("parsing.int.expected").at(self));
        }
        number.parse().map_err(|_| {
            self.cursor = start;
            CommandError::dynamic("parsing.int.invalid", [number]).at(self)
        })
    }

    pub fn read_long(&mut self) -> Result<i64, CommandError> {
        let start = self.cursor;
        let number = self.read_while(is_number_char);
        if number.is_empty() {
            return Err(CommandError::simple("parsing.long.expected").at(self));
        }
        number.parse().map_err(|_| {
            self.cursor = start;
            CommandError::dynamic("parsing.long.invalid", [number]).at(self)
        })
    }

    pub fn read_double(&mut self) -> Result<f64, CommandError> {
        let start = self.cursor;
        let number = self.read_while(is_number_char);
        if number.is_empty() {
            return Err(CommandError::simple("parsing.double.expected").at(self));
        }
        parse_float(number).map_err(|_| {
            self.cursor = start;
            CommandError::dynamic("parsing.double.invalid", [number]).at(self)
        })
    }

    pub fn read_float(&mut self) -> Result<f32, CommandError> {
        let start = self.cursor;
        let number = self.read_while(is_number_char);
        if number.is_empty() {
            return Err(CommandError::simple("parsing.float.expected").at(self));
        }
        parse_float(number).map(|v| v as f32).map_err(|_| {
            self.cursor = start;
            CommandError::dynamic("parsing.float.invalid", [number]).at(self)
        })
    }

    pub fn read_unquoted_string(&mut self) -> &'a str {
        self.read_while(is_unquoted_char)
    }

    pub fn read_quoted_string(&mut self) -> Result<String, CommandError> {
        let Some(quote) = self.peek().filter(|c| is_quote(*c)) else {
            if !self.can_read() {
                return Ok(String::new());
            }
            return Err(CommandError::simple("parsing.quote.expected.start").at(self));
        };
        self.skip();
        self.read_string_until(quote)
    }

    pub fn read_string_until(&mut self, terminator: char) -> Result<String, CommandError> {
        let mut result = String::new();
        let mut escaped = false;
        while let Some(c) = self.read() {
            if escaped {
                if c == terminator || c == '\\' {
                    result.push(c);
                    escaped = false;
                } else {
                    self.cursor -= c.len_utf8();
                    return Err(
                        CommandError::dynamic("parsing.quote.escape", [c.to_string()]).at(self),
                    );
                }
            } else if c == '\\' {
                escaped = true;
            } else if c == terminator {
                return Ok(result);
            } else {
                result.push(c);
            }
        }
        Err(CommandError::simple("parsing.quote.expected.end").at(self))
    }

    /// A quoted string if one starts here, else an unquoted one.
    pub fn read_string(&mut self) -> Result<String, CommandError> {
        if self.peek().is_some_and(is_quote) {
            return self.read_quoted_string();
        }
        Ok(self.read_unquoted_string().to_owned())
    }

    pub fn read_bool(&mut self) -> Result<bool, CommandError> {
        let start = self.cursor;
        let value = self.read_string()?;
        if value.is_empty() {
            return Err(CommandError::simple("parsing.bool.expected").at(self));
        }
        match value.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => {
                self.cursor = start;
                Err(CommandError::dynamic("parsing.bool.invalid", [value]).at(self))
            }
        }
    }

    pub fn expect(&mut self, c: char) -> Result<(), CommandError> {
        if self.peek_is(c) {
            self.skip();
            Ok(())
        } else {
            Err(CommandError::dynamic("parsing.expected", [c.to_string()]).at(self))
        }
    }
}

/// Parses a number the way Java's `Double.parseDouble` accepts the
/// characters the reader lets through (digits, `.` and `-`).
fn parse_float(number: &str) -> Result<f64, ()> {
    let unsigned = number.strip_prefix('-').unwrap_or(number);
    if unsigned.is_empty() || unsigned.starts_with('-') || unsigned == "." {
        return Err(());
    }
    number.parse().map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(err: CommandError) -> String {
        err.message.translate.unwrap().into_string()
    }

    #[tuff_core::watchdog]
    #[test]
    fn reads_numbers_words_and_quotes() {
        let mut r = Reader::new("12 -3.5 word \"quoted \\\"x\\\"\" true");
        assert_eq!(r.read_int().unwrap(), 12);
        r.skip();
        assert_eq!(r.read_double().unwrap(), -3.5);
        r.skip();
        assert_eq!(r.read_unquoted_string(), "word");
        r.skip();
        assert_eq!(r.read_string().unwrap(), "quoted \"x\"");
        r.skip();
        assert!(r.read_bool().unwrap());
        assert!(!r.can_read());
    }

    #[tuff_core::watchdog]
    #[test]
    fn reports_the_games_keys() {
        assert_eq!(
            key(Reader::new("x").read_int().unwrap_err()),
            "parsing.int.expected"
        );
        assert_eq!(
            key(Reader::new("1.5").read_int().unwrap_err()),
            "parsing.int.invalid"
        );
        assert_eq!(
            key(Reader::new("-").read_double().unwrap_err()),
            "parsing.double.invalid"
        );
        assert_eq!(
            key(Reader::new("").read_float().unwrap_err()),
            "parsing.float.expected"
        );
        assert_eq!(
            key(Reader::new("").read_bool().unwrap_err()),
            "parsing.bool.expected"
        );
        assert_eq!(
            key(Reader::new("yes").read_bool().unwrap_err()),
            "parsing.bool.invalid"
        );
        assert_eq!(
            key(Reader::new("\"abc").read_string().unwrap_err()),
            "parsing.quote.expected.end"
        );
        assert_eq!(
            key(Reader::new("\"a\\q\"").read_string().unwrap_err()),
            "parsing.quote.escape"
        );
        assert_eq!(
            key(Reader::new("b").expect('a').unwrap_err()),
            "parsing.expected"
        );
        let mut r = Reader::new("1.5 x");
        let err = r.read_int().unwrap_err();
        assert_eq!(err.cursor, Some(0), "the cursor rolls back to the number");
    }
}
