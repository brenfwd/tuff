//! Renders a binary recording as text lines:
//! `<unix ns> <state> <sb|cb> <text>`. Serverbound text is the decoded
//! packet's Debug output; clientbound text is the typed Debug from
//! [`dump::Decoder`] followed by ` | ` and the framed form
//! `Packet { name, id, len, data }`. Events render as `# ...` lines.

use std::io::Read;

use super::format::{self, FormatError, Reader, Record};
use super::{ConnState, Direction, format_line, raw_text};

/// Typed packet text supplied by whoever owns the packet definitions;
/// `None` falls back to the framed raw form.
pub trait Decoder {
    fn clientbound(&self, state: ConnState, payload: &bytes::Bytes) -> Option<String>;
    fn serverbound(&self, state: ConnState, payload: &bytes::Bytes) -> Option<String>;
}

/// A decoder that never decodes, leaving only the framed raw form.
pub struct RawOnly;

impl Decoder for RawOnly {
    fn clientbound(&self, _: ConnState, _: &bytes::Bytes) -> Option<String> {
        None
    }

    fn serverbound(&self, _: ConnState, _: &bytes::Bytes) -> Option<String> {
        None
    }
}

const MAX_TEXT: usize = 4096;

/// Options for [`render`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Leave out packets flagged as chat.
    pub no_chat: bool,
}

/// Renders every record of `input`, calling `line` with each text line
/// (newline included). Returns the number of records rendered.
pub fn render(
    input: impl Read,
    options: Options,
    decoder: &dyn Decoder,
    mut line: impl FnMut(&str),
) -> Result<u64, FormatError> {
    let mut reader = Reader::new(input)?;
    let start = u128::from(reader.header().start_unix_ns);
    let mut count = 0;
    while let Some(block) = reader.next_block()? {
        for record in block {
            if options.no_chat && record.tag.is_chat() {
                continue;
            }
            let ns = start.saturating_add_signed(i128::from(record.ns));
            line(&render_record(ns, &record, decoder));
            count += 1;
        }
    }
    if reader.truncated() {
        line("# truncated: the recording ends without an end marker\n");
    }
    Ok(count)
}

/// Renders a whole recording to a string.
pub fn render_to_string(
    input: impl Read,
    options: Options,
    decoder: &dyn Decoder,
) -> Result<String, FormatError> {
    let mut out = String::new();
    render(input, options, decoder, |l| out.push_str(l))?;
    Ok(out)
}

fn render_record(ns: u128, record: &Record, decoder: &dyn Decoder) -> String {
    if record.tag.is_event() {
        return format!("# {}\n", event_text(record));
    }
    let state = record.tag.state();
    let direction = record.tag.direction();
    let mut text = match direction {
        Direction::Serverbound => serverbound_text(decoder, state, record),
        Direction::Clientbound => clientbound_text(decoder, state, record),
    };
    if record.tag.is_redacted() {
        text.push_str(" [redacted]");
    }
    if text.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("..");
    }
    format_line(ns, state, direction, &text)
}

fn event_text(record: &Record) -> String {
    let kind = record.payload.first().copied();
    let value = format::get_varint(record.payload.get(1..).unwrap_or_default());
    match (kind, value) {
        (Some(format::EVENT_CONNECTION), Some(n)) => format!("connection {n}"),
        (Some(format::EVENT_DROPPED), Some(n)) => format!("dropped {n} records"),
        _ => format!(
            "event {:?}",
            &record.payload[..record.payload.len().min(16)]
        ),
    }
}

fn clientbound_text(decoder: &dyn Decoder, state: ConnState, record: &Record) -> String {
    let raw = raw_text(state, Direction::Clientbound, record.payload.clone());
    if record.tag.is_redacted() {
        return raw;
    }
    match decoder.clientbound(state, &record.payload) {
        Some(typed) => format!("{typed} | {raw}"),
        None => raw,
    }
}

fn serverbound_text(decoder: &dyn Decoder, state: ConnState, record: &Record) -> String {
    if !record.tag.is_redacted()
        && let Some(typed) = decoder.serverbound(state, &record.payload)
    {
        return typed;
    }
    raw_text(state, Direction::Serverbound, record.payload.clone())
}
