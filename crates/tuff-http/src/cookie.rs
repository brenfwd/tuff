//! `Set-Cookie` values built from checked parts, and request cookie lookup.

use hyper::header::{HeaderName, HeaderValue};

use super::{Request, Response};

const MAX_VALUE: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
}

#[derive(Clone, Debug)]
pub struct Cookie {
    name: String,
    value: String,
    path: Option<String>,
    max_age: Option<u64>,
    http_only: bool,
    secure: bool,
    same_site: Option<SameSite>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CookieError {
    Name,
    Value,
    Path,
}

impl std::fmt::Display for CookieError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self {
            Self::Name => "name",
            Self::Value => "value",
            Self::Path => "path",
        };
        write!(f, "unsafe cookie {what}")
    }
}

impl std::error::Error for CookieError {}

fn is_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn is_cookie_octet(byte: u8) -> bool {
    matches!(byte, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e)
}

impl Cookie {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            path: None,
            max_age: None,
            http_only: false,
            secure: false,
            same_site: None,
        }
    }

    /// A cookie that tells the browser to drop `name` at `path`.
    pub fn removal(name: impl Into<String>) -> Self {
        Self::new(name, "").max_age(0)
    }

    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn max_age(mut self, seconds: u64) -> Self {
        self.max_age = Some(seconds);
        self
    }

    pub fn http_only(mut self) -> Self {
        self.http_only = true;
        self
    }

    pub fn secure(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }

    pub fn same_site(mut self, same_site: SameSite) -> Self {
        self.same_site = Some(same_site);
        self
    }

    /// The `Set-Cookie` header value, refusing any part that could break
    /// out of its attribute.
    pub fn header_value(&self) -> Result<HeaderValue, CookieError> {
        if self.name.is_empty() || !self.name.bytes().all(is_token) {
            return Err(CookieError::Name);
        }
        if self.value.len() > MAX_VALUE || !self.value.bytes().all(is_cookie_octet) {
            return Err(CookieError::Value);
        }
        let mut out = format!("{}={}", self.name, self.value);
        if let Some(path) = &self.path {
            if !path.starts_with('/')
                || !path.bytes().all(|b| (0x20..0x7f).contains(&b) && b != b';')
            {
                return Err(CookieError::Path);
            }
            out.push_str("; Path=");
            out.push_str(path);
        }
        if let Some(age) = self.max_age {
            out.push_str(&format!("; Max-Age={age}"));
        }
        if self.http_only {
            out.push_str("; HttpOnly");
        }
        if self.secure {
            out.push_str("; Secure");
        }
        match self.same_site {
            Some(SameSite::Strict) => out.push_str("; SameSite=Strict"),
            Some(SameSite::Lax) => out.push_str("; SameSite=Lax"),
            None => {}
        }
        HeaderValue::from_str(&out).map_err(|_| CookieError::Value)
    }
}

impl Response {
    pub fn set_cookie(mut self, cookie: &Cookie) -> Result<Self, CookieError> {
        let value = cookie.header_value()?;
        self.headers
            .push((HeaderName::from_static("set-cookie"), value));
        Ok(self)
    }
}

impl Request {
    /// The first value of cookie `name` across every `Cookie` header.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.headers
            .get_all("cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.trim_matches('"'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn builds_flags_and_refuses_unsafe_parts() {
        let c = Cookie::new("tuff_web", "abc123")
            .path("/admin")
            .http_only()
            .secure(true)
            .same_site(SameSite::Strict)
            .max_age(60);
        assert_eq!(
            c.header_value().unwrap(),
            "tuff_web=abc123; Path=/admin; Max-Age=60; HttpOnly; Secure; SameSite=Strict"
        );
        assert_eq!(
            Cookie::new("a", "b; Domain=evil").header_value(),
            Err(CookieError::Value)
        );
        assert_eq!(
            Cookie::new("a b", "x").header_value(),
            Err(CookieError::Name)
        );
        assert_eq!(Cookie::new("", "x").header_value(), Err(CookieError::Name));
        assert_eq!(
            Cookie::new("a", "x\r\nSet-Cookie: y").header_value(),
            Err(CookieError::Value)
        );
        assert_eq!(
            Cookie::new("a", "x").path("/a;Secure").header_value(),
            Err(CookieError::Path)
        );
        assert_eq!(
            Cookie::new("a", "\"q\"").header_value(),
            Err(CookieError::Value)
        );
    }
}
