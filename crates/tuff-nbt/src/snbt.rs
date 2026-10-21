use super::*;

/// `{}` formats compact SNBT; `{:#}` formats indented SNBT.
impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let indent = f.alternate().then_some(0);
        write_snbt(self, f, indent)
    }
}

impl fmt::Display for Compound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let indent = f.alternate().then_some(0);
        write_compound_snbt(self, f, indent)
    }
}

pub(super) fn write_snbt(
    tag: &Tag,
    f: &mut fmt::Formatter<'_>,
    indent: Option<usize>,
) -> fmt::Result {
    match tag {
        Tag::Byte(v) => write!(f, "{v}b"),
        Tag::Short(v) => write!(f, "{v}s"),
        Tag::Int(v) => write!(f, "{v}"),
        Tag::Long(v) => write!(f, "{v}L"),
        Tag::Float(v) => write!(
            f,
            "{}f",
            java_float(f64::from(*v), format!("{v:?}"), format!("{v:E}"))
        ),
        Tag::Double(v) => write!(f, "{}d", java_float(*v, format!("{v:?}"), format!("{v:E}"))),
        Tag::String(s) => write_quoted(s, f),
        Tag::ByteArray(v) => write_array(f, 'B', v, "b", indent),
        Tag::IntArray(v) => write_array(f, 'I', v, "", indent),
        Tag::LongArray(v) => write_array(f, 'L', v, "L", indent),
        Tag::List(items) => write_seq(f, ['[', ']'], items.iter(), indent, |f, item, indent| {
            write_snbt(item, f, indent)
        }),
        Tag::Compound(c) => write_compound_snbt(c, f, indent),
    }
}

pub(super) fn write_compound_snbt(
    c: &Compound,
    f: &mut fmt::Formatter<'_>,
    indent: Option<usize>,
) -> fmt::Result {
    let mut sorted: Vec<_> = c.iter().collect();
    sorted.sort_unstable_by(|a, b| a.0.cmp(b.0));
    write_seq(
        f,
        ['{', '}'],
        sorted.into_iter(),
        indent,
        |f, (key, value), indent| {
            if is_bare_key(key) {
                f.write_str(key)?;
            } else {
                write_quoted(key, f)?;
            }
            f.write_str(if indent.is_some() { ": " } else { ":" })?;
            write_snbt(value, f, indent)
        },
    )
}

pub(super) fn write_seq<T>(
    f: &mut fmt::Formatter<'_>,
    [open, close]: [char; 2],
    items: impl ExactSizeIterator<Item = T>,
    indent: Option<usize>,
    mut each: impl FnMut(&mut fmt::Formatter<'_>, T, Option<usize>) -> fmt::Result,
) -> fmt::Result {
    let len = items.len();
    let inner = indent.map(|level| level + 1);
    f.write_char(open)?;
    for (i, item) in items.enumerate() {
        if let Some(level) = inner {
            write!(f, "\n{:1$}", "", level * 4)?;
        }
        each(f, item, inner)?;
        if i + 1 < len {
            f.write_char(',')?;
        }
    }
    if let Some(level) = indent.filter(|_| len > 0) {
        write!(f, "\n{:1$}", "", level * 4)?;
    }
    f.write_char(close)
}

pub(super) fn write_array<T: fmt::Display>(
    f: &mut fmt::Formatter<'_>,
    prefix: char,
    items: &[T],
    suffix: &str,
    indent: Option<usize>,
) -> fmt::Result {
    write!(f, "[{prefix};")?;
    for (i, v) in items.iter().enumerate() {
        if i > 0 {
            f.write_char(',')?;
        }
        if indent.is_some() {
            f.write_char(' ')?;
        }
        write!(f, "{v}{suffix}")?;
    }
    f.write_char(']')
}

pub(super) fn write_quoted(s: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let quote = match s.chars().find(|c| matches!(c, '"' | '\'')) {
        Some('"') => '\'',
        _ => '"',
    };
    f.write_char(quote)?;
    for c in s.chars() {
        match c {
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\t' => f.write_str("\\t")?,
            '\r' => f.write_str("\\r")?,
            '\u{8}' => f.write_str("\\b")?,
            '\u{c}' => f.write_str("\\f")?,
            c if c == quote => write!(f, "\\{c}")?,
            c if c.is_ascii_control() => write!(f, "\\x{:02x}", u32::from(c))?,
            c => f.write_char(c)?,
        }
    }
    f.write_char(quote)
}

pub(super) fn is_bare_key(key: &str) -> bool {
    let b = key.as_bytes();
    let head = b
        .iter()
        .take_while(|c| c.is_ascii_alphabetic() || matches!(c, b'.' | b'_'))
        .count();
    head > 0 && b[head..].iter().all(|&c| is_word_byte(c)) && key != "true" && key != "false"
}

pub(super) fn java_float(value: f64, plain: String, sci: String) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let abs = value.abs();
    if abs == 0.0 || (1e-3..1e7).contains(&abs) {
        return if plain.contains('e') || plain.contains('E') {
            sci
        } else {
            plain
        };
    }
    match sci.split_once('E') {
        Some((m, e)) if m.contains('.') => format!("{m}E{e}"),
        Some((m, e)) => format!("{m}.0E{e}"),
        None => sci,
    }
}

pub(super) fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'+')
}

pub(super) struct SnbtParser<'a> {
    pub(super) src: &'a str,
    pub(super) pos: usize,
    pub(super) depth: usize,
}

impl<'a> SnbtParser<'a> {
    pub(super) fn err<T>(&self, reason: &'static str) -> Result<T> {
        Err(NbtError::Snbt {
            position: self.pos,
            reason,
        })
    }

    pub(super) fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    pub(super) fn skip_ws(&mut self) {
        let rest = &self.src[self.pos..];
        self.pos += rest.len() - rest.trim_start().len();
    }

    pub(super) fn eat(&mut self, c: char) -> bool {
        let found = self.peek() == Some(c);
        if found {
            self.pos += c.len_utf8();
        }
        found
    }

    pub(super) fn expect(&mut self, c: char, reason: &'static str) -> Result<()> {
        if self.eat(c) {
            Ok(())
        } else {
            self.err(reason)
        }
    }

    pub(super) fn word(&mut self) -> &'a str {
        let rest = &self.src[self.pos..];
        let len = rest.bytes().take_while(|&b| is_word_byte(b)).count();
        self.pos += len;
        &rest[..len]
    }

    pub(super) fn nested<T>(&mut self, parse: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.depth >= MAX_DEPTH {
            return Err(NbtError::DepthLimit(MAX_DEPTH));
        }
        self.depth += 1;
        let result = parse(self);
        self.depth -= 1;
        result
    }

    pub(super) fn value(&mut self) -> Result<Tag> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.nested(|p| p.compound().map(Tag::Compound)),
            Some('[') => self.nested(Self::list),
            Some('"' | '\'') => self.quoted().map(Tag::String),
            _ => self.literal(),
        }
    }

    pub(super) fn key(&mut self) -> Result<String> {
        if matches!(self.peek(), Some('"' | '\'')) {
            return self.quoted();
        }
        match self.word() {
            "" => self.err("expected key"),
            word => Ok(word.to_owned()),
        }
    }

    pub(super) fn compound(&mut self) -> Result<Compound> {
        self.pos += 1;
        let mut compound = Compound::new();
        loop {
            self.skip_ws();
            if self.eat('}') {
                return Ok(compound);
            }
            let key = self.key()?;
            self.skip_ws();
            self.expect(':', "expected ':'")?;
            let value = self.value()?;
            compound.insert(key, value);
            self.skip_ws();
            if !self.eat(',') {
                self.expect('}', "expected ',' or '}'")?;
                return Ok(compound);
            }
        }
    }

    pub(super) fn list(&mut self) -> Result<Tag> {
        self.pos += 1;
        if let [kind @ (b'B' | b'I' | b'L'), b';', ..] = self.src.as_bytes()[self.pos..] {
            self.pos += 2;
            return Ok(match kind {
                b'B' => Tag::ByteArray(self.elements(Self::integer)?),
                b'I' => Tag::IntArray(self.elements(Self::integer)?),
                _ => Tag::LongArray(self.elements(Self::integer)?),
            });
        }
        self.elements(Self::value).map(Tag::List)
    }

    pub(super) fn elements<T>(
        &mut self,
        mut item: impl FnMut(&mut Self) -> Result<T>,
    ) -> Result<Vec<T>> {
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.eat(']') {
                return Ok(items);
            }
            items.push(item(self)?);
            self.skip_ws();
            if !self.eat(',') {
                self.expect(']', "expected ',' or ']'")?;
                return Ok(items);
            }
        }
    }

    pub(super) fn integer<T: TryFrom<i64>>(&mut self) -> Result<T> {
        self.skip_ws();
        let start = self.pos;
        let value = match self.value()? {
            Tag::Byte(v) => i64::from(v),
            Tag::Short(v) => i64::from(v),
            Tag::Int(v) => i64::from(v),
            Tag::Long(v) => v,
            _ => {
                self.pos = start;
                return self.err("expected integer array element");
            }
        };
        T::try_from(value).or_else(|_| {
            self.pos = start;
            self.err("array element out of range")
        })
    }

    pub(super) fn quoted(&mut self) -> Result<String> {
        let quote = char::from(self.src.as_bytes()[self.pos]);
        self.pos += 1;
        let mut out = String::new();
        loop {
            let rest = &self.src[self.pos..];
            let Some(i) = rest.find([quote, '\\']) else {
                return self.err("unterminated string");
            };
            out.push_str(&rest[..i]);
            self.pos += i + 1;
            if rest.as_bytes()[i] == quote as u8 {
                return Ok(out);
            }
            out.push(self.escape()?);
        }
    }

    pub(super) fn escape(&mut self) -> Result<char> {
        let Some(c) = self.peek() else {
            return self.err("unterminated string");
        };
        self.pos += c.len_utf8();
        let digits = match c {
            '\\' | '\'' | '"' => return Ok(c),
            'b' => return Ok('\u{8}'),
            's' => return Ok(' '),
            't' => return Ok('\t'),
            'n' => return Ok('\n'),
            'f' => return Ok('\u{c}'),
            'r' => return Ok('\r'),
            'x' => 2,
            'u' => 4,
            'U' => 8,
            _ => return self.err("invalid escape sequence"),
        };
        let decoded = self
            .src
            .get(self.pos..self.pos + digits)
            .filter(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .and_then(char::from_u32);
        match decoded {
            Some(c) => {
                self.pos += digits;
                Ok(c)
            }
            None => self.err("invalid escape sequence"),
        }
    }

    pub(super) fn literal(&mut self) -> Result<Tag> {
        let start = self.pos;
        let word = self.word();
        if word.is_empty() {
            return self.err("expected value");
        }
        if self.peek() == Some('(') {
            return self.operation(word, start);
        }
        match word {
            "true" => Ok(Tag::Byte(1)),
            "false" => Ok(Tag::Byte(0)),
            _ => match parse_number(word) {
                Some(Ok(tag)) => Ok(tag),
                Some(Err(reason)) => Err(NbtError::Snbt {
                    position: start,
                    reason,
                }),
                None => Ok(Tag::String(word.to_owned())),
            },
        }
    }

    pub(super) fn operation(&mut self, name: &str, start: usize) -> Result<Tag> {
        self.pos += 1;
        let arg_start = self.pos;
        let arg = self.value()?;
        let tag = match (name, &arg) {
            ("bool", _) => match arg.as_number() {
                Some(n) => Tag::Byte((n != 0.0).into()),
                None => {
                    self.pos = arg_start;
                    return self.err("bool() expects a number");
                }
            },
            ("uuid", Tag::String(s)) => match parse_uuid(s) {
                Some(uuid) => Tag::uuid(uuid),
                None => {
                    self.pos = arg_start;
                    return self.err("invalid UUID");
                }
            },
            ("uuid", _) => {
                self.pos = arg_start;
                return self.err("uuid() expects a string");
            }
            _ => {
                self.pos = start;
                return self.err("unknown operation");
            }
        };
        self.skip_ws();
        self.expect(')', "expected ')'")?;
        Ok(tag)
    }
}

pub(super) fn parse_uuid(s: &str) -> Option<u128> {
    let groups: Vec<&str> = s.split('-').collect();
    let well_formed = groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|g| g.bytes().all(|b| b.is_ascii_hexdigit()));
    well_formed.then(|| u128::from_str_radix(&groups.concat(), 16).ok())?
}

pub(super) fn valid_digits(digits: &str) -> bool {
    !digits.starts_with('_') && !digits.ends_with('_')
}

pub(super) fn strip_digits(s: &str, radix: u32) -> (&str, &str) {
    s.split_at(
        s.find(|c: char| !(c.is_digit(radix) || c == '_'))
            .unwrap_or(s.len()),
    )
}

/// Integer suffix: optional signedness (`u`/`s`) followed by a width letter.
pub(super) fn int_suffix(suffix: &str) -> Option<(Option<bool>, u32)> {
    let bits = |c: u8| match c.to_ascii_lowercase() {
        b'b' => Some(8),
        b's' => Some(16),
        b'i' => Some(32),
        b'l' => Some(64),
        _ => None,
    };
    match *suffix.as_bytes() {
        [] => Some((None, 32)),
        [w] => Some((None, bits(w)?)),
        [s, w] => {
            let unsigned = match s.to_ascii_lowercase() {
                b'u' => true,
                b's' => false,
                _ => return None,
            };
            Some((Some(unsigned), bits(w)?))
        }
        _ => None,
    }
}

/// Returns `None` when `word` is not shaped like a number, so the caller can
/// treat it as an unquoted string.
pub(super) fn parse_number(word: &str) -> Option<std::result::Result<Tag, &'static str>> {
    let (negative, body) = match word.as_bytes().first()? {
        b'-' => (true, &word[1..]),
        b'+' => (false, &word[1..]),
        _ => (false, word),
    };
    let radix = match body.as_bytes() {
        [b'0', b'x' | b'X', ..] => 16,
        [b'0', b'b' | b'B', b'0' | b'1', ..] => 2,
        _ => 10,
    };
    if radix != 10 {
        let (digits, suffix) = strip_digits(&body[2..], radix);
        let (unsigned, bits) = int_suffix(suffix)?;
        if digits.is_empty() || !valid_digits(digits) {
            return None;
        }
        return Some(parse_integer(negative, digits, radix, unsigned, bits));
    }

    let (int_part, mut rest) = strip_digits(body, 10);
    let mut frac_part = None;
    if let Some(after_dot) = rest.strip_prefix('.') {
        let (frac, after) = strip_digits(after_dot, 10);
        frac_part = Some(frac);
        rest = after;
    }
    let mut exponent = None;
    if let Some(after_e) = rest.strip_prefix(['e', 'E']) {
        let sign_len = usize::from(after_e.starts_with(['+', '-']));
        let (digits, after) = strip_digits(&after_e[sign_len..], 10);
        if digits.is_empty() || !valid_digits(digits) {
            return None;
        }
        exponent = Some(&after_e[..sign_len + digits.len()]);
        rest = after;
    }
    let frac = frac_part.unwrap_or("");
    if (int_part.is_empty() && frac.is_empty()) || !valid_digits(int_part) || !valid_digits(frac) {
        return None;
    }

    let is_float = frac_part.is_some() || exponent.is_some();
    let double = match rest {
        "f" | "F" => false,
        "d" | "D" => true,
        "" if is_float => true,
        _ if is_float => return None,
        _ => {
            let (unsigned, bits) = int_suffix(rest)?;
            return Some(parse_integer(negative, int_part, 10, unsigned, bits));
        }
    };
    let mut text: String = format!(
        "{}{int_part}.{frac}e{}",
        if negative { "-" } else { "" },
        exponent.unwrap_or("0")
    );
    text.retain(|c| c != '_');
    Some(Ok(if double {
        Tag::Double(text.parse().ok()?)
    } else {
        Tag::Float(text.parse().ok()?)
    }))
}

pub(super) fn parse_integer(
    negative: bool,
    digits: &str,
    radix: u32,
    unsigned: Option<bool>,
    bits: u32,
) -> std::result::Result<Tag, &'static str> {
    pub(super) const OUT_OF_RANGE: &str = "integer out of range";
    let clean: Cow<'_, str> = if digits.contains('_') {
        Cow::Owned(digits.replace('_', ""))
    } else {
        Cow::Borrowed(digits)
    };
    let magnitude = i128::from(u64::from_str_radix(&clean, radix).map_err(|_| OUT_OF_RANGE)?);
    let value = if negative { -magnitude } else { magnitude };
    let signed_min = -(1i128 << (bits - 1));
    let signed_max = (1i128 << (bits - 1)) - 1;
    let unsigned_max = (1i128 << bits) - 1;
    let in_range = match unsigned {
        Some(true) => (0..=unsigned_max).contains(&value),
        None if radix != 10 => (signed_min..=unsigned_max).contains(&value),
        _ => (signed_min..=signed_max).contains(&value),
    };
    if !in_range {
        return Err(OUT_OF_RANGE);
    }
    let raw = value as i64;
    Ok(match bits {
        8 => Tag::Byte(raw as i8),
        16 => Tag::Short(raw as i16),
        32 => Tag::Int(raw as i32),
        _ => Tag::Long(raw),
    })
}
