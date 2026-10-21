// SPDX-License-Identifier: MIT
//! Outbound HTTP as a future: `http::get(url).await`. The request is
//! checked against the plugin's `cap.http` manifest lines and the owner's
//! grants; a refusal comes back as the [`Error`].

use crate::bindings::tuff::plugin::capability_types as cap;
use crate::bindings::tuff::plugin::net;
use crate::exec::{Call, Completion};

pub use cap::Denied as Error;

/// A finished HTTP exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
    /// The URL that answered, after redirects.
    pub url: String,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The first header called `name`, ignoring case, as text.
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
    }
}

impl From<cap::HttpResponse> for Response {
    fn from(response: cap::HttpResponse) -> Self {
        Self {
            status: response.status,
            headers: response
                .headers
                .into_iter()
                .map(|h| (h.name, h.value))
                .collect(),
            body: response.body,
            url: response.url,
        }
    }
}

/// An HTTP request under construction.
#[derive(Debug, Clone)]
pub struct Request {
    inner: cap::HttpRequest,
}

impl Request {
    pub fn new(method: &str, url: &str) -> Self {
        Self {
            inner: cap::HttpRequest {
                method: method.to_owned(),
                url: url.to_owned(),
                headers: Vec::new(),
                body: Vec::new(),
                timeout_ms: None,
                decompress: false,
                max_redirects: 0,
            },
        }
    }

    pub fn header(mut self, name: &str, value: impl AsRef<[u8]>) -> Self {
        self.inner.headers.push(cap::Header {
            name: name.to_owned(),
            value: value.as_ref().to_vec(),
        });
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.inner.body = body.into();
        self
    }

    /// Sets the body to `json` (already serialized) with its content type.
    pub fn json(self, json: impl Into<Vec<u8>>) -> Self {
        self.header("content-type", "application/json").body(json)
    }

    /// Shorter than the owner's limit only.
    pub fn timeout_ms(mut self, ms: u32) -> Self {
        self.inner.timeout_ms = Some(ms);
        self
    }

    /// Asks for gzip and inflates it.
    pub fn decompress(mut self) -> Self {
        self.inner.decompress = true;
        self
    }

    pub fn max_redirects(mut self, n: u8) -> Self {
        self.inner.max_redirects = n;
        self
    }

    /// Sends the request. Refusals the host can see up front fail right
    /// away; the rest answer when the exchange finishes.
    pub async fn send(self) -> Result<Response, Error> {
        let handle = net::fetch(&self.inner)?;
        match Call::http(handle).await {
            Completion::Http(outcome) => outcome.map(Response::from),
            _ => Err(Error::Failed("mismatched completion".into())),
        }
    }
}

pub async fn get(url: &str) -> Result<Response, Error> {
    Request::new("GET", url).send().await
}

pub async fn post(url: &str, body: impl Into<Vec<u8>>) -> Result<Response, Error> {
    Request::new("POST", url).body(body).send().await
}

/// Quotes `text` as a JSON string, for hand-built request bodies.
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
