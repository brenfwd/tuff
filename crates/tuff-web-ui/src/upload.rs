//! Small uploads read into memory (data and resource packs): the size is
//! declared up front and checked against the cap before any byte is read,
//! and the body may not run past it.

use std::path::Path;

use hyper::StatusCode;
use serde_json::Value;

use serde_json::json;
use tuff_http::{Request, Response};

fn error(status: StatusCode, message: &str) -> Response {
    Response::json(status, &json!({ "error": message }))
}

const MAX_SHOWN_NAME: usize = 128;

/// The declared size of an upload of at most `cap` bytes.
pub fn declared_length(req: &Request, cap: u64, what: &str) -> Result<u64, Response> {
    let Some(len) = req
        .header("content-length")
        .and_then(|v| v.trim().parse::<u64>().ok())
    else {
        return Err(error(
            StatusCode::LENGTH_REQUIRED,
            "send the file with a Content-Length",
        ));
    };
    if len == 0 {
        return Err(error(StatusCode::BAD_REQUEST, "the file is empty"));
    }
    if len > cap {
        return Err(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("{what} over {} MiB cannot be uploaded", cap >> 20),
        ));
    }
    Ok(len)
}

/// Reads exactly `len` bytes of the request's streamed body. Memory grows
/// with what arrives, never with what was declared.
pub async fn receive(req: &mut Request, len: u64) -> Result<Vec<u8>, Response> {
    let Some(mut body) = req.take_body_stream() else {
        return Err(error(StatusCode::INTERNAL_SERVER_ERROR, "no request body"));
    };
    let mut bytes = Vec::new();
    loop {
        match body.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len() as u64 + chunk.len() as u64 > len {
                    return Err(error(
                        StatusCode::BAD_REQUEST,
                        "the body is longer than its Content-Length",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(err) => return Err(error(err.response().status, &err.to_string())),
        }
    }
    if bytes.len() as u64 != len {
        return Err(error(StatusCode::BAD_REQUEST, "the upload ended early"));
    }
    Ok(bytes)
}

/// Refuses an upload of `len` bytes when `dir`'s disk has less than twice
/// that free. Blocking.
pub fn check_space(dir: &Path, len: u64) -> Result<(), Response> {
    let needed = len.saturating_mul(2);
    match tuff_storage::backup::space::available(dir) {
        Some(free) if free < needed => Err(error(
            StatusCode::INSUFFICIENT_STORAGE,
            &format!("not enough free disk space: {needed} bytes needed, {free} free"),
        )),
        _ => Ok(()),
    }
}

/// The client's name for the uploaded file, for display only.
pub fn shown_name(req: &Request) -> String {
    req.query_param("filename")
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_SHOWN_NAME)
        .collect()
}

/// The error message of a refusal, for the audit log.
pub fn refusal(resp: &Response) -> String {
    resp.body
        .bytes()
        .and_then(|b| serde_json::from_slice::<Value>(b).ok())
        .and_then(|v| v["error"].as_str().map(str::to_owned))
        .unwrap_or_else(|| resp.status.to_string())
}

/// A download's `content-disposition` for `name`, which callers keep to
/// plain file name characters.
pub fn attachment(resp: &mut Response, name: &str) {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect();
    resp.set_header(
        "content-disposition",
        &format!("attachment; filename=\"{safe}\""),
    );
}

/// Whether a query flag is set (`1` or `true`).
pub fn flag(req: &Request, name: &str) -> bool {
    matches!(req.query_param(name).as_deref(), Some("1" | "true"))
}
